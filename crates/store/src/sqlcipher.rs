//! Encrypted history store (SQLCipher), enabled with `--features sqlcipher`.
//!
//! The master key is generated once (256 random bits) and stored in the OS
//! credential store (Windows Credential Manager, macOS Keychain, or Secret
//! Service). The database is unreadable without it, and no copy of the key is
//! kept on disk.

use std::path::Path;
use std::sync::Mutex;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use rand::RngCore;
use rusqlite::types::Value;
use rusqlite::{params, params_from_iter, Connection};
use serde::{Deserialize, Serialize};

use restbook_core::store::{trim_target, Draft};
use restbook_core::{
    Body, Collection, Environment, Error, HistoryEntry, HistoryFilter, HistoryHit, Kv, Method,
    Origin, RedirectHop, RequestDef, ResponseRecord, Result, Store,
};

const SERVICE: &str = "restbook";
const ACCOUNT: &str = "master-key";

/// `settings` keys for the small scalars that do not deserve their own table.
const SETTING_ACTIVE_ENVIRONMENT: &str = "active_environment";
const SETTING_GLOBALS: &str = "globals";
/// Marks that `history_fts` was already filled from the existing history, so a
/// reopen does not index the same executions twice.
const SETTING_FTS_BACKFILLED: &str = "fts_backfilled";

/// Contentless (`content=''`) so the index holds no second copy of the history,
/// keyed by the execution id as its rowid. The trigram tokenizer provides the
/// substring search that `LIKE '%q%'` used to.
///
/// Not part of `SCHEMA`: a build without FTS5/trigram must still open the
/// database, so this runs separately and only success marks the store indexed.
const CREATE_HISTORY_FTS: &str = "CREATE VIRTUAL TABLE IF NOT EXISTS history_fts \
     USING fts5(url, method, name, content='', tokenize='trigram');";

/// Index for owner-scoped history. Not part of `SCHEMA`: an old database has
/// neither origin column until the `ALTER`s in `open` add them, and the batch
/// would fail on the unknown column.
const CREATE_EXECUTIONS_ORIGIN_INDEX: &str = "CREATE INDEX IF NOT EXISTS executions_origin \
     ON executions(origin_kind, origin_id, received_at DESC);";

const SCHEMA: &str = r#"
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS requests (
    id           INTEGER PRIMARY KEY,
    method       TEXT NOT NULL,
    url          TEXT NOT NULL,
    url_key      TEXT NOT NULL UNIQUE,
    name         TEXT NOT NULL,
    body         TEXT NOT NULL,
    request_json TEXT NOT NULL DEFAULT '',
    updated_at   INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS executions (
    id          INTEGER PRIMARY KEY,
    request_id  INTEGER NOT NULL REFERENCES requests(id),
    status      INTEGER NOT NULL,
    duration_ms INTEGER NOT NULL,
    size_bytes  INTEGER NOT NULL,
    received_at INTEGER NOT NULL,
    response    TEXT NOT NULL,
    -- Who owned the request when it was sent: `standalone`, `saved`, or
    -- `draft`; `origin_id` is the node id or the draft row id. Old databases
    -- gain these columns in `open` (SQLite has no ADD COLUMN IF NOT EXISTS).
    origin_kind TEXT NOT NULL DEFAULT 'standalone',
    origin_id   TEXT NOT NULL DEFAULT ''
);

CREATE INDEX IF NOT EXISTS executions_received_at ON executions(received_at DESC);

CREATE TABLE IF NOT EXISTS collections (
    id         TEXT PRIMARY KEY,
    name       TEXT NOT NULL,
    json       TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS environments (
    name       TEXT PRIMARY KEY,
    json       TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS drafts (
    id           INTEGER PRIMARY KEY,
    request_json TEXT NOT NULL,
    updated_at   INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
"#;

pub struct SqlcipherStore {
    conn: Mutex<Connection>,
    /// `true` only when `history_fts` exists and was usable at open time. When
    /// it is `false` the store keeps the original `LIKE` scan everywhere, so a
    /// SQLCipher build without FTS5/trigram behaves exactly as before.
    fts: bool,
}

impl SqlcipherStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_key(path, &load_or_create_key()?)
    }

    /// Open with a key the caller already holds. Production code reaches this
    /// through [`open`], which takes the key from the OS keyring; the tests pin
    /// one instead, because a headless machine may have no credential store to
    /// read it back from.
    fn open_with_key(path: impl AsRef<Path>, key: &[u8; 32]) -> Result<Self> {
        let hex: String = key.iter().map(|byte| format!("{byte:02x}")).collect();

        let mut conn = Connection::open(path.as_ref()).map_err(db_error)?;
        // The key pragma must run before anything else touches the database;
        // the blob-literal form avoids quoting issues for arbitrary key bytes.
        // `cipher_memory_security = OFF` is deliberate: it does not weaken the
        // encrypted-at-rest guarantee, but cuts per-statement cost, which
        // matters because search runs synchronously on UI changes.
        conn.execute_batch(&format!(
            "PRAGMA key = \"x'{hex}'\";\n\
             PRAGMA cipher_memory_security = OFF;\n\
             PRAGMA journal_mode = WAL;\n\
             PRAGMA synchronous = NORMAL;\n\
             PRAGMA temp_store = MEMORY;"
        ))
        .map_err(db_error)?;
        conn.execute_batch(SCHEMA).map_err(db_error)?;
        // Databases created before `request_json` existed already have the
        // table, so the `CREATE` above was a no-op. SQLite has no
        // `ADD COLUMN IF NOT EXISTS`; the duplicate-column error is expected.
        let _ = conn.execute(
            "ALTER TABLE requests ADD COLUMN request_json TEXT NOT NULL DEFAULT ''",
            [],
        );
        // Ownership arrived after the first schema, so existing executions are
        // backfilled as `standalone` by the column default — the same meaning
        // `Origin::default()` gives a legacy row on the other backends.
        let _ = conn.execute(
            "ALTER TABLE executions ADD COLUMN origin_kind TEXT NOT NULL DEFAULT 'standalone'",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE executions ADD COLUMN origin_id TEXT NOT NULL DEFAULT ''",
            [],
        );
        // Only meaningful once both columns exist. A failure (for example a
        // database opened read-only) leaves owner lookup on a table scan rather
        // than taking the store down.
        let _ = conn.execute_batch(CREATE_EXECUTIONS_ORIGIN_INDEX);

        // Capability detection as a fallback, never as a hard error: a failed
        // `CREATE` here just means the history search stays on `LIKE`.
        let fts = ensure_history_fts(&mut conn);

        Ok(Self {
            conn: Mutex::new(conn),
            fts,
        })
    }
}

/// Creates `history_fts` when this build supports it and, in that case, fills it
/// from the already-stored history exactly once. Returns whether the index is
/// usable; any error means "no FTS", which the callers treat as a downgrade.
fn ensure_history_fts(conn: &mut Connection) -> bool {
    if conn.execute_batch(CREATE_HISTORY_FTS).is_err() {
        return false;
    }

    let backfilled = match conn.query_row(
        "SELECT 1 FROM settings WHERE key = ?1",
        params![SETTING_FTS_BACKFILLED],
        |_| Ok(()),
    ) {
        Ok(()) => true,
        Err(rusqlite::Error::QueryReturnedNoRows) => false,
        Err(_) => return false,
    };

    // The marker is the source of truth: present means the index was built from
    // this database before (rows written since then were indexed on insert).
    // Absent means every execution needs indexing, including databases that
    // predate the index. The empty table `CREATE` just made is fine to rebuild.
    if backfilled {
        return true;
    }

    backfill_history_fts(conn).is_ok()
}

fn backfill_history_fts(conn: &mut Connection) -> rusqlite::Result<()> {
    let tx = conn.transaction()?;
    // A contentless FTS5 table cannot `DELETE` from or `rebuild`, so a fresh
    // backfill is a drop and re-create. Inside the transaction a partially
    // written index is never visible, and the marker commits with the rows.
    rebuild_history_fts(&tx)?;
    tx.execute(
        "INSERT INTO settings (key, value) VALUES (?1, '1')
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![SETTING_FTS_BACKFILLED],
    )?;
    tx.commit()
}

/// Rebuild the contentless `history_fts` from the executions still in the table.
/// It cannot `DELETE` individual rows, so every mutation that removes history —
/// retention, a single-send delete, a clear — drops and re-creates it inside the
/// caller's transaction so the index can never outlive its rows.
fn rebuild_history_fts(tx: &rusqlite::Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch("DROP TABLE IF EXISTS history_fts;")?;
    tx.execute_batch(CREATE_HISTORY_FTS)?;
    tx.execute(
        "INSERT INTO history_fts(rowid, url, method, name)
         SELECT e.id, r.url, r.method, r.name
         FROM executions e
         JOIN requests r ON r.id = e.request_id",
        [],
    )?;
    Ok(())
}

impl Store for SqlcipherStore {
    fn append_history(&self, entry: &HistoryEntry) -> Result<i64> {
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| Error::Store("history lock poisoned".to_owned()))?;

        let url_key = format!("{} {}", entry.request.method, entry.url);
        let received_at = entry.response.received_at.timestamp_millis();
        let (origin_kind, origin_id) = origin_parts(&entry.origin);

        let stored_response = StoredResponse {
            status: entry.response.status,
            headers: entry.response.headers.clone(),
            body_b64: BASE64.encode(entry.response.body.as_bytes()),
            duration_ms: entry.response.duration_ms,
            received_at,
            final_url: entry.response.final_url.clone(),
            redirects: entry.response.redirects.clone(),
        };

        // The history row and its search index row are written together so they
        // cannot drift: either both land or neither does.
        let tx = conn.transaction().map_err(db_error)?;

        tx.execute(
            "INSERT INTO requests (method, url, url_key, name, body, request_json, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(url_key) DO UPDATE SET
                 method       = excluded.method,
                 name         = excluded.name,
                 body         = excluded.body,
                 request_json = excluded.request_json,
                 updated_at   = excluded.updated_at",
            params![
                entry.request.method.as_str(),
                entry.url,
                url_key,
                entry.request.name,
                serde_json::to_string(&entry.request.body)?,
                serde_json::to_string(&entry.request)?,
                received_at,
            ],
        )
        .map_err(db_error)?;

        let request_id: i64 = tx
            .query_row(
                "SELECT id FROM requests WHERE url_key = ?1",
                params![url_key],
                |row| row.get(0),
            )
            .map_err(db_error)?;

        tx.execute(
            "INSERT INTO executions
                 (request_id, status, duration_ms, size_bytes, received_at, response,
                  origin_kind, origin_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                request_id,
                entry.response.status,
                entry.response.duration_ms as i64,
                entry.response.body.len() as i64,
                received_at,
                serde_json::to_string(&stored_response)?,
                origin_kind,
                origin_id,
            ],
        )
        .map_err(db_error)?;

        let execution_id = tx.last_insert_rowid();

        if self.fts {
            // The execution id is the rowid, which is the only identity a
            // contentless FTS5 table keeps. It is fresh from the insert above,
            // so this can never hit an existing rowid.
            tx.execute(
                "INSERT INTO history_fts(rowid, url, method, name) VALUES (?1, ?2, ?3, ?4)",
                params![
                    execution_id,
                    entry.url,
                    entry.request.method.as_str(),
                    entry.request.name,
                ],
            )
            .map_err(db_error)?;
        }

        tx.commit().map_err(db_error)?;
        Ok(execution_id)
    }

    fn search_history(
        &self,
        query: &str,
        filter: &HistoryFilter,
        limit: u32,
    ) -> Result<Vec<HistoryHit>> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| Error::Store("history lock poisoned".to_owned()))?;

        let trimmed = query.trim();
        // The trigram tokenizer indexes three-character windows, so anything
        // shorter cannot be answered by the index and keeps the scan.
        if self.fts && trimmed.chars().count() >= 3 {
            // A corrupt or half-written index must not break search, so any FTS
            // error falls through to the `LIKE` scan below.
            if let Ok(hits) = search_history_fts(&conn, trimmed, filter, limit) {
                return Ok(hits);
            }
        }

        search_history_like(&conn, trimmed, filter, limit)
    }

    fn get_history(&self, id: i64) -> Result<Option<HistoryEntry>> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| Error::Store("history lock poisoned".to_owned()))?;

        let row = conn.query_row(
            "SELECT r.method, r.url, r.name, r.body, r.request_json, e.response,
                    e.origin_kind, e.origin_id
             FROM executions e
             JOIN requests r ON r.id = e.request_id
             WHERE e.id = ?1",
            params![id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                ))
            },
        );

        let (method, url, name, body, request_json, response_json, origin_kind, origin_id) =
            match row {
                Ok(row) => row,
                Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
                Err(error) => return Err(db_error(error)),
            };

        // Rows written before `request_json` existed can still restore the
        // method, URL, and body; the other tabs did not survive in that schema.
        let request = if request_json.is_empty() {
            let mut request =
                RequestDef::new(Method::parse(&method).unwrap_or(Method::Get), url.clone());
            request.name = name;
            request.body = serde_json::from_str::<Body>(&body).unwrap_or_default();
            request
        } else {
            serde_json::from_str(&request_json)
                .map_err(|error| Error::Store(format!("stored request: {error}")))?
        };

        let stored: StoredResponse = serde_json::from_str(&response_json)
            .map_err(|error| Error::Store(format!("stored response: {error}")))?;
        let body = BASE64
            .decode(&stored.body_b64)
            .map_err(|error| Error::Store(format!("stored response body: {error}")))?;
        let received_at =
            chrono::DateTime::from_timestamp_millis(stored.received_at).unwrap_or_default();

        Ok(Some(HistoryEntry {
            id,
            request,
            url,
            origin: origin_from_parts(&origin_kind, &origin_id),
            response: ResponseRecord {
                status: stored.status,
                headers: stored.headers,
                body: body.into(),
                duration_ms: stored.duration_ms,
                received_at,
                final_url: stored.final_url,
                redirects: stored.redirects,
            },
        }))
    }

    fn history_for_origin(
        &self,
        origin: &Origin,
        filter: &HistoryFilter,
        limit: u32,
    ) -> Result<Option<Vec<HistoryHit>>> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| Error::Store("history lock poisoned".to_owned()))?;

        let (kind, id) = origin_parts(origin);
        let mut sql = String::from(
            "SELECT e.id, r.method, r.url, e.status, e.received_at
             FROM executions e
             JOIN requests r ON r.id = e.request_id
             WHERE e.origin_kind = ?1 AND e.origin_id = ?2",
        );
        let mut values = vec![Value::Text(kind.to_owned()), Value::Text(id)];
        let mut next = 2;
        push_filter(&mut sql, &mut values, filter, &mut next);
        next += 1;
        sql.push_str(&format!(
            " ORDER BY e.received_at DESC, e.id DESC LIMIT ?{next}"
        ));
        values.push(Value::Integer(limit as i64));

        let mut statement = conn.prepare(&sql).map_err(db_error)?;
        let rows = statement
            .query_map(params_from_iter(values), history_hit)
            .map_err(db_error)?;

        let mut hits = Vec::new();
        for row in rows {
            hits.push(row.map_err(db_error)?);
        }
        Ok(Some(hits))
    }

    fn reassign_history_origin(&self, from: &Origin, to: &Origin) -> Result<u64> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| Error::Store("history lock poisoned".to_owned()))?;

        let (from_kind, from_id) = origin_parts(from);
        let (to_kind, to_id) = origin_parts(to);
        // One statement, so a crash cannot leave half a draft's sends under the
        // old owner. The FTS index does not carry ownership, so it is untouched.
        let updated = conn
            .execute(
                "UPDATE executions SET origin_kind = ?1, origin_id = ?2
                 WHERE origin_kind = ?3 AND origin_id = ?4",
                params![to_kind, to_id, from_kind, from_id],
            )
            .map_err(db_error)?;
        Ok(updated as u64)
    }

    fn trim_history(&self, keep: u32) -> Result<u64> {
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| Error::Store("history lock poisoned".to_owned()))?;
        let tx = conn.transaction().map_err(db_error)?;

        // Retention is by age, not id: `received_at` is indexed, and an imported
        // execution can carry an older timestamp than the one before it.
        let removed = tx
            .execute(
                "DELETE FROM executions WHERE id NOT IN (
                     SELECT id FROM executions
                     ORDER BY received_at DESC, id DESC
                     LIMIT ?1
                 )",
                params![trim_target(keep) as i64],
            )
            .map_err(db_error)?;

        if removed == 0 {
            tx.commit().map_err(db_error)?;
            return Ok(0);
        }

        // `history_fts` is contentless, so it cannot `DELETE` the pruned rows.
        // Rebuild it from what is left in the same transaction, exactly as the
        // open-time backfill does, so the index tracks the trimmed history.
        if self.fts {
            tx.execute_batch("DROP TABLE IF EXISTS history_fts;")
                .map_err(db_error)?;
            tx.execute_batch(CREATE_HISTORY_FTS).map_err(db_error)?;
            tx.execute(
                "INSERT INTO history_fts(rowid, url, method, name)
                 SELECT e.id, r.url, r.method, r.name
                 FROM executions e
                 JOIN requests r ON r.id = e.request_id",
                [],
            )
            .map_err(db_error)?;
        }

        // A request row only exists to be joined by its executions. Once the
        // last execution for one is gone, its `request_json` is dead weight too.
        tx.execute(
            "DELETE FROM requests WHERE id NOT IN (SELECT DISTINCT request_id FROM executions)",
            [],
        )
        .map_err(db_error)?;

        tx.commit().map_err(db_error)?;
        Ok(removed as u64)
    }

    fn delete_history(&self, id: i64) -> Result<bool> {
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| Error::Store("history lock poisoned".to_owned()))?;
        let tx = conn.transaction().map_err(db_error)?;

        let removed = tx
            .execute("DELETE FROM executions WHERE id = ?1", params![id])
            .map_err(db_error)?;
        if removed == 0 {
            tx.commit().map_err(db_error)?;
            return Ok(false);
        }

        // The request row only exists to be joined by its executions; the last
        // one going away orphans it.
        tx.execute(
            "DELETE FROM requests WHERE id NOT IN (SELECT DISTINCT request_id FROM executions)",
            [],
        )
        .map_err(db_error)?;
        if self.fts {
            rebuild_history_fts(&tx).map_err(db_error)?;
        }

        tx.commit().map_err(db_error)?;
        Ok(true)
    }

    fn clear_history(&self) -> Result<u64> {
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| Error::Store("history lock poisoned".to_owned()))?;
        let tx = conn.transaction().map_err(db_error)?;

        let removed = tx.execute("DELETE FROM executions", []).map_err(db_error)?;
        if removed == 0 {
            tx.commit().map_err(db_error)?;
            return Ok(0);
        }

        tx.execute("DELETE FROM requests", []).map_err(db_error)?;
        if self.fts {
            rebuild_history_fts(&tx).map_err(db_error)?;
        }

        tx.commit().map_err(db_error)?;
        Ok(removed as u64)
    }

    fn delete_history_for_origin(&self, origin: &Origin) -> Result<u64> {
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| Error::Store("history lock poisoned".to_owned()))?;
        let (kind, id) = origin_parts(origin);
        let tx = conn.transaction().map_err(db_error)?;

        let removed = tx
            .execute(
                "DELETE FROM executions WHERE origin_kind = ?1 AND origin_id = ?2",
                params![kind, id],
            )
            .map_err(db_error)?;
        if removed == 0 {
            tx.commit().map_err(db_error)?;
            return Ok(0);
        }

        tx.execute(
            "DELETE FROM requests WHERE id NOT IN (SELECT DISTINCT request_id FROM executions)",
            [],
        )
        .map_err(db_error)?;
        if self.fts {
            rebuild_history_fts(&tx).map_err(db_error)?;
        }

        tx.commit().map_err(db_error)?;
        Ok(removed as u64)
    }

    fn collections(&self) -> Result<Vec<Collection>> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| Error::Store("history lock poisoned".to_owned()))?;

        let mut statement = conn
            .prepare("SELECT json FROM collections ORDER BY name")
            .map_err(db_error)?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(db_error)?;

        let mut collections = Vec::new();
        for row in rows {
            let json = row.map_err(db_error)?;
            collections.push(
                serde_json::from_str(&json)
                    .map_err(|error| Error::Store(format!("stored collection: {error}")))?,
            );
        }
        Ok(collections)
    }

    fn save_collection(&self, collection: &Collection) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| Error::Store("history lock poisoned".to_owned()))?;

        conn.execute(
            "INSERT INTO collections (id, name, json, updated_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(id) DO UPDATE SET
                 name       = excluded.name,
                 json       = excluded.json,
                 updated_at = excluded.updated_at",
            params![
                collection.id,
                collection.name,
                serde_json::to_string(collection)?,
                now_ms(),
            ],
        )
        .map_err(db_error)?;
        Ok(())
    }

    fn delete_collection(&self, id: &str) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| Error::Store("history lock poisoned".to_owned()))?;
        conn.execute("DELETE FROM collections WHERE id = ?1", params![id])
            .map_err(db_error)?;
        Ok(())
    }

    fn environments(&self) -> Result<Vec<Environment>> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| Error::Store("history lock poisoned".to_owned()))?;

        let mut statement = conn
            .prepare("SELECT json FROM environments ORDER BY name")
            .map_err(db_error)?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(db_error)?;

        let mut environments = Vec::new();
        for row in rows {
            let json = row.map_err(db_error)?;
            environments.push(
                serde_json::from_str(&json)
                    .map_err(|error| Error::Store(format!("stored environment: {error}")))?,
            );
        }
        Ok(environments)
    }

    fn save_environment(&self, environment: &Environment) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| Error::Store("history lock poisoned".to_owned()))?;

        conn.execute(
            "INSERT INTO environments (name, json, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(name) DO UPDATE SET
                 json       = excluded.json,
                 updated_at = excluded.updated_at",
            params![
                environment.name,
                serde_json::to_string(environment)?,
                now_ms(),
            ],
        )
        .map_err(db_error)?;
        Ok(())
    }

    fn delete_environment(&self, name: &str) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| Error::Store("history lock poisoned".to_owned()))?;
        conn.execute("DELETE FROM environments WHERE name = ?1", params![name])
            .map_err(db_error)?;
        Ok(())
    }

    fn active_environment(&self) -> Result<Option<String>> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| Error::Store("history lock poisoned".to_owned()))?;

        match conn.query_row(
            "SELECT value FROM settings WHERE key = ?1",
            params![SETTING_ACTIVE_ENVIRONMENT],
            |row| row.get::<_, String>(0),
        ) {
            Ok(value) => Ok(Some(value)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(error) => Err(db_error(error)),
        }
    }

    fn set_active_environment(&self, name: Option<&str>) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| Error::Store("history lock poisoned".to_owned()))?;

        match name {
            Some(name) => {
                conn.execute(
                    "INSERT INTO settings (key, value) VALUES (?1, ?2)
                     ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                    params![SETTING_ACTIVE_ENVIRONMENT, name],
                )
                .map_err(db_error)?;
            }
            None => {
                conn.execute(
                    "DELETE FROM settings WHERE key = ?1",
                    params![SETTING_ACTIVE_ENVIRONMENT],
                )
                .map_err(db_error)?;
            }
        }
        Ok(())
    }

    fn globals(&self) -> Result<Vec<Kv>> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| Error::Store("history lock poisoned".to_owned()))?;

        let json = match conn.query_row(
            "SELECT value FROM settings WHERE key = ?1",
            params![SETTING_GLOBALS],
            |row| row.get::<_, String>(0),
        ) {
            Ok(json) => json,
            Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(Vec::new()),
            Err(error) => return Err(db_error(error)),
        };

        serde_json::from_str(&json)
            .map_err(|error| Error::Store(format!("stored globals: {error}")))
    }

    fn save_globals(&self, vars: &[Kv]) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| Error::Store("history lock poisoned".to_owned()))?;

        conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![SETTING_GLOBALS, serde_json::to_string(vars)?],
        )
        .map_err(db_error)?;
        Ok(())
    }

    fn drafts(&self) -> Result<Vec<Draft>> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| Error::Store("history lock poisoned".to_owned()))?;

        let mut statement = conn
            .prepare("SELECT id, request_json, updated_at FROM drafts ORDER BY updated_at DESC")
            .map_err(db_error)?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })
            .map_err(db_error)?;

        let mut drafts = Vec::new();
        for row in rows {
            let (id, request_json, updated_at) = row.map_err(db_error)?;
            drafts.push(Draft {
                id,
                request: serde_json::from_str(&request_json)
                    .map_err(|error| Error::Store(format!("stored draft: {error}")))?,
                updated_at: chrono::DateTime::from_timestamp_millis(updated_at).unwrap_or_default(),
            });
        }
        Ok(drafts)
    }

    fn save_draft(&self, draft: &Draft) -> Result<i64> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| Error::Store("history lock poisoned".to_owned()))?;

        let request_json = serde_json::to_string(&draft.request)?;
        let updated_at = draft.updated_at.timestamp_millis();

        // An id of `0` means "not stored yet", exactly as on `HistoryEntry`.
        if draft.id == 0 {
            conn.execute(
                "INSERT INTO drafts (request_json, updated_at) VALUES (?1, ?2)",
                params![request_json, updated_at],
            )
            .map_err(db_error)?;
            return Ok(conn.last_insert_rowid());
        }

        conn.execute(
            "INSERT INTO drafts (id, request_json, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(id) DO UPDATE SET
                 request_json = excluded.request_json,
                 updated_at   = excluded.updated_at",
            params![draft.id, request_json, updated_at],
        )
        .map_err(db_error)?;
        Ok(draft.id)
    }

    fn delete_draft(&self, id: i64) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| Error::Store("history lock poisoned".to_owned()))?;
        conn.execute("DELETE FROM drafts WHERE id = ?1", params![id])
            .map_err(db_error)?;
        Ok(())
    }
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// The `LIKE` scan, kept verbatim: it is the fallback when the FTS index is
/// unavailable or cannot be trusted for the query (fewer than 3 characters).
/// An empty query becomes `%%`, which matches everything: most recent first.
fn search_history_like(
    conn: &Connection,
    query: &str,
    filter: &HistoryFilter,
    limit: u32,
) -> Result<Vec<HistoryHit>> {
    let pattern = format!("%{}%", query.to_ascii_lowercase());

    let mut sql = String::from(
        "SELECT e.id, r.method, r.url, e.status, e.received_at
         FROM executions e
         JOIN requests r ON r.id = e.request_id
         WHERE (lower(r.url) LIKE ?1
            OR lower(r.method) LIKE ?1
            OR lower(r.name) LIKE ?1)",
    );
    let mut values = vec![Value::Text(pattern)];
    let mut next = 1;
    push_filter(&mut sql, &mut values, filter, &mut next);
    next += 1;
    sql.push_str(&format!(
        " ORDER BY e.received_at DESC, e.id DESC LIMIT ?{next}"
    ));
    values.push(Value::Integer(limit as i64));

    let mut statement = conn.prepare(&sql).map_err(db_error)?;
    let rows = statement
        .query_map(params_from_iter(values), history_hit)
        .map_err(db_error)?;

    let mut hits = Vec::new();
    for row in rows {
        hits.push(row.map_err(db_error)?);
    }
    Ok(hits)
}

/// Indexed substring search. The query is escaped into a single FTS5 phrase, so
/// any FTS operator the user typed (`"`, `*`, `-`, `NEAR`) is taken literally
/// and can never turn into a syntax error or broaden the match.
fn search_history_fts(
    conn: &Connection,
    query: &str,
    filter: &HistoryFilter,
    limit: u32,
) -> Result<Vec<HistoryHit>> {
    let phrase = format!("\"{}\"", query.replace('"', "\"\""));

    let mut sql = String::from(
        "SELECT e.id, r.method, r.url, e.status, e.received_at
         FROM history_fts f
         JOIN executions e ON e.id = f.rowid
         JOIN requests r ON r.id = e.request_id
         WHERE f MATCH ?1",
    );
    let mut values = vec![Value::Text(phrase)];
    let mut next = 1;
    push_filter(&mut sql, &mut values, filter, &mut next);
    next += 1;
    sql.push_str(&format!(
        " ORDER BY e.received_at DESC, e.id DESC LIMIT ?{next}"
    ));
    values.push(Value::Integer(limit as i64));

    let mut statement = conn.prepare(&sql).map_err(db_error)?;
    let rows = statement
        .query_map(params_from_iter(values), history_hit)
        .map_err(db_error)?;

    let mut hits = Vec::new();
    for row in rows {
        hits.push(row.map_err(db_error)?);
    }
    Ok(hits)
}

/// Append the filter's conditions to `sql`, continuing the `?N` numbering from
/// `next`. Values are bound, never interpolated.
fn push_filter(
    sql: &mut String,
    values: &mut Vec<Value>,
    filter: &HistoryFilter,
    next: &mut usize,
) {
    if !filter.methods.is_empty() {
        sql.push_str(" AND upper(r.method) IN (");
        for (index, method) in filter.methods.iter().enumerate() {
            if index > 0 {
                sql.push_str(", ");
            }
            *next += 1;
            sql.push_str(&format!("?{next}"));
            values.push(Value::Text(method.to_ascii_uppercase()));
        }
        sql.push(')');
    }

    if !filter.status_classes.is_empty() {
        sql.push_str(" AND (e.status / 100) IN (");
        for (index, class) in filter.status_classes.iter().enumerate() {
            if index > 0 {
                sql.push_str(", ");
            }
            *next += 1;
            sql.push_str(&format!("?{next}"));
            values.push(Value::Integer(i64::from(*class)));
        }
        sql.push(')');
    }

    if let Some(since) = filter.since {
        *next += 1;
        sql.push_str(&format!(" AND e.received_at >= ?{next}"));
        values.push(Value::Integer(since.timestamp_millis()));
    }

    if let Some(until) = filter.until {
        *next += 1;
        sql.push_str(&format!(" AND e.received_at <= ?{next}"));
        values.push(Value::Integer(until.timestamp_millis()));
    }
}

fn history_hit(row: &rusqlite::Row<'_>) -> rusqlite::Result<HistoryHit> {
    let received_at: i64 = row.get(4)?;
    Ok(HistoryHit {
        id: row.get(0)?,
        method: row.get(1)?,
        url: row.get(2)?,
        status: row.get(3)?,
        at: chrono::DateTime::from_timestamp_millis(received_at)
            .map(|at| at.to_rfc3339())
            .unwrap_or_default(),
    })
}

/// The `(kind, id)` pair an execution stores for its owner. `Standalone` keeps
/// an empty id so the pair is a stable, indexable key.
fn origin_parts(origin: &Origin) -> (&'static str, String) {
    match origin {
        Origin::Standalone => ("standalone", String::new()),
        Origin::Saved { node } => ("saved", node.clone()),
        Origin::Draft { draft } => ("draft", draft.to_string()),
    }
}

/// The inverse of [`origin_parts`]. An unknown kind (a hand-edited database, or
/// one written by a newer build) reads as `Standalone`, which changes nothing
/// except falling back to a URL search.
fn origin_from_parts(kind: &str, id: &str) -> Origin {
    match kind {
        "saved" if !id.is_empty() => Origin::Saved {
            node: id.to_owned(),
        },
        "draft" => id
            .parse::<i64>()
            .map(|draft| Origin::Draft { draft })
            .unwrap_or_default(),
        _ => Origin::Standalone,
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct StoredResponse {
    status: u16,
    headers: Vec<(String, String)>,
    body_b64: String,
    duration_ms: u64,
    received_at: i64,
    // Added with the redirect record. Responses stored before it load with
    // `None`/empty, which is exactly what an un-redirected response means.
    #[serde(default)]
    final_url: Option<String>,
    #[serde(default)]
    redirects: Vec<RedirectHop>,
}

fn db_error(error: rusqlite::Error) -> Error {
    Error::Store(error.to_string())
}

fn load_or_create_key() -> Result<[u8; 32]> {
    let entry = keyring::Entry::new(SERVICE, ACCOUNT)
        .map_err(|error| Error::Store(format!("keyring: {error}")))?;

    match entry.get_password() {
        Ok(encoded) => decode_key(&encoded),
        Err(keyring::Error::NoEntry) => {
            let mut key = [0u8; 32];
            rand::thread_rng().fill_bytes(&mut key);
            let encoded: String = key.iter().map(|byte| format!("{byte:02x}")).collect();
            entry
                .set_password(&encoded)
                .map_err(|error| Error::Store(format!("keyring: {error}")))?;
            Ok(key)
        }
        Err(error) => Err(Error::Store(format!("keyring: {error}"))),
    }
}

fn decode_key(encoded: &str) -> Result<[u8; 32]> {
    let bytes = encoded.trim().as_bytes();
    if bytes.len() != 64 {
        return Err(Error::Store(
            "stored master key has an unexpected length".to_owned(),
        ));
    }

    let mut key = [0u8; 32];
    for (index, byte) in key.iter_mut().enumerate() {
        let pair = std::str::from_utf8(&bytes[index * 2..index * 2 + 2])
            .map_err(|_| Error::Store("stored master key is not hex".to_owned()))?;
        *byte = u8::from_str_radix(pair, 16)
            .map_err(|_| Error::Store("stored master key is not hex".to_owned()))?;
    }
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use restbook_core::{CollectionItem, Method, RequestDef, ResponseRecord, SavedRequest};

    /// `load_or_create_key` races when the keyring entry does not exist yet and
    /// two tests start at once, so the encrypted tests take a turn each.
    static KEYRING_LOCK: Mutex<()> = Mutex::new(());

    fn exclusive() -> std::sync::MutexGuard<'static, ()> {
        KEYRING_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// A key every test database opens with. The real path takes it from the OS
    /// keyring, which a headless machine or CI runner may not have, and
    /// `keyring`'s own mock store keeps nothing between entries — useless for a
    /// test that reopens a database.
    const TEST_KEY: [u8; 32] = [0x5a; 32];

    fn open_test(path: impl AsRef<Path>) -> Result<SqlcipherStore> {
        SqlcipherStore::open_with_key(path, &TEST_KEY)
    }

    fn sample_entry() -> HistoryEntry {
        let mut request = RequestDef::new(Method::Post, "https://example.com/things");
        request.headers.push(restbook_core::Kv::new("x-test", "1"));
        request.body = Body::Raw {
            mime: "application/json".to_owned(),
            text: "{\"ok\":true}".to_owned(),
        };

        HistoryEntry {
            id: 0,
            url: request.url.clone(),
            request,
            response: ResponseRecord {
                status: 200,
                headers: vec![("content-type".to_owned(), "application/json".to_owned())],
                body: br#"{"ok":true}"#.to_vec().into(),
                duration_ms: 12,
                received_at: chrono::Utc::now(),
                final_url: None,
                redirects: Vec::new(),
            },
            origin: Origin::default(),
        }
    }

    #[test]
    fn roundtrips_history_through_an_encrypted_database() {
        let _guard = exclusive();

        let dir = std::env::temp_dir().join("restbook-sqlcipher-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let store = open_test(dir.join("history.db")).unwrap();
        let id = store.append_history(&sample_entry()).unwrap();
        assert!(id > 0);

        let hits = store
            .search_history("example.com", &HistoryFilter::default(), 10)
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].status, 200);
        assert_eq!(hits[0].url, "https://example.com/things");

        // The hit is only a row; the full request comes back through
        // `get_history`, which is what a history click restores.
        let loaded = store.get_history(hits[0].id).unwrap().unwrap();
        assert_eq!(loaded.request.method, Method::Post);
        assert_eq!(loaded.request.headers.len(), 1);
        assert!(matches!(loaded.request.body, Body::Raw { .. }));
        assert_eq!(loaded.response.status, 200);
    }

    /// Two saved requests with the same URL are different owners, so they do not
    /// share a send log. Ownership is stored on the execution and is what a
    /// draft's sends are moved over to when the draft is filed.
    #[test]
    fn origins_scope_and_move_history_in_an_encrypted_database() {
        let _guard = exclusive();
        let dir = test_dir("restbook-sqlcipher-origin-test");
        let db = dir.join("history.db");
        let store = open_test(&db).unwrap();

        let url = "https://example.com/things";
        let mut earlier = entry_named(Method::Get, url, "things", 1_700_000_000_000);
        earlier.origin = Origin::Saved {
            node: "node-a".to_owned(),
        };
        let mut later = entry_named(Method::Get, url, "things", 1_700_000_001_000);
        later.origin = Origin::Saved {
            node: "node-a".to_owned(),
        };
        let mut sibling = entry_named(Method::Get, url, "things", 1_700_000_002_000);
        sibling.origin = Origin::Saved {
            node: "node-b".to_owned(),
        };
        let mut draft = entry_named(Method::Get, url, "things", 1_700_000_003_000);
        draft.origin = Origin::Draft { draft: 5 };
        let unowned = entry_named(Method::Get, url, "things", 1_700_000_004_000);

        let earlier_id = store.append_history(&earlier).unwrap();
        let later_id = store.append_history(&later).unwrap();
        store.append_history(&sibling).unwrap();
        store.append_history(&draft).unwrap();
        store.append_history(&unowned).unwrap();

        // The owner survives the round trip through `get_history`.
        assert_eq!(
            store.get_history(earlier_id).unwrap().unwrap().origin,
            Origin::Saved {
                node: "node-a".to_owned()
            }
        );

        let node_a = Origin::Saved {
            node: "node-a".to_owned(),
        };
        let rows = store
            .history_for_origin(&node_a, &HistoryFilter::default(), 10)
            .unwrap()
            .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, later_id, "newest first, like the URL search");

        // Ownership narrows the list; it does not hide rows from a URL search.
        assert_eq!(
            store
                .search_history("example.com", &HistoryFilter::default(), 10)
                .unwrap()
                .len(),
            5
        );

        // Filing the draft moves only its own row.
        let from = Origin::Draft { draft: 5 };
        let to = Origin::Saved {
            node: "node-c".to_owned(),
        };
        assert_eq!(store.reassign_history_origin(&from, &to).unwrap(), 1);
        assert!(store
            .history_for_origin(&from, &HistoryFilter::default(), 10)
            .unwrap()
            .unwrap()
            .is_empty());
        assert_eq!(
            store
                .history_for_origin(&to, &HistoryFilter::default(), 10)
                .unwrap()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            store
                .history_for_origin(&Origin::Standalone, &HistoryFilter::default(), 10)
                .unwrap()
                .unwrap()
                .len(),
            1,
            "the unsaved send is still unowned"
        );

        // The owners are on disk, not in memory.
        drop(store);
        let reopened = open_test(&db).unwrap();
        assert_eq!(
            reopened
                .history_for_origin(&node_a, &HistoryFilter::default(), 10)
                .unwrap()
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            reopened.get_history(earlier_id).unwrap().unwrap().origin,
            node_a
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Removing a send must also take its row out of the contentless search
    /// index, or a later search would return an id that no longer resolves.
    #[test]
    fn deleting_and_clearing_sends_in_an_encrypted_database() {
        let _guard = exclusive();
        let dir = test_dir("restbook-sqlcipher-delete-test");
        let db = dir.join("history.db");
        let store = open_test(&db).unwrap();

        let a = entry_named(
            Method::Get,
            "https://example.com/one",
            "one",
            1_700_000_000_000,
        );
        let b = entry_named(
            Method::Get,
            "https://example.com/two",
            "two",
            1_700_000_001_000,
        );
        let mut owned = entry_named(
            Method::Get,
            "https://example.com/two",
            "two",
            1_700_000_002_000,
        );
        owned.origin = Origin::Saved {
            node: "node-a".to_owned(),
        };
        let a_id = store.append_history(&a).unwrap();
        let b_id = store.append_history(&b).unwrap();
        store.append_history(&owned).unwrap();

        assert!(store.delete_history(b_id).unwrap());
        assert!(
            !store.delete_history(b_id).unwrap(),
            "a second delete is a no-op"
        );
        assert!(store.get_history(b_id).unwrap().is_none());
        // The trigram index no longer returns the deleted row.
        assert_eq!(
            store
                .search_history("two", &HistoryFilter::default(), 10)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            store
                .search_history("one", &HistoryFilter::default(), 10)
                .unwrap()
                .len(),
            1
        );
        assert!(store.get_history(a_id).unwrap().is_some());

        // An owner-scoped clear forgets only that owner's sends.
        assert_eq!(
            store
                .delete_history_for_origin(&Origin::Saved {
                    node: "node-a".to_owned(),
                })
                .unwrap(),
            1
        );
        assert_eq!(
            store
                .search_history("", &HistoryFilter::default(), 10)
                .unwrap()
                .len(),
            1
        );

        assert_eq!(store.clear_history().unwrap(), 1);
        assert!(store
            .search_history("", &HistoryFilter::default(), 10)
            .unwrap()
            .is_empty());

        // The deletions are on disk, and the index agrees on reopen.
        drop(store);
        let reopened = open_test(&db).unwrap();
        assert!(reopened
            .search_history("", &HistoryFilter::default(), 10)
            .unwrap()
            .is_empty());
        assert!(reopened.get_history(a_id).unwrap().is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A database written before ownership existed gets the columns on open and
    /// its rows read as `Standalone` — the URL fallback, not lost history.
    #[test]
    fn an_old_database_gains_ownership_columns_on_open() {
        let _guard = exclusive();
        let dir = test_dir("restbook-sqlcipher-migration-test");
        let db = dir.join("history.db");

        let id = {
            let store = open_test(&db).unwrap();
            store.append_history(&sample_entry()).unwrap()
        };

        // Strip the feature back to the previous schema, index included: the
        // index references the columns, so SQLite refuses to drop them first.
        {
            let store = open_test(&db).unwrap();
            let conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            conn.execute_batch(
                "DROP INDEX IF EXISTS executions_origin;
                 ALTER TABLE executions DROP COLUMN origin_id;
                 ALTER TABLE executions DROP COLUMN origin_kind;",
            )
            .expect("the bundled SQLite supports DROP COLUMN");
        }

        let store = open_test(&db).unwrap();
        assert_eq!(
            store.get_history(id).unwrap().unwrap().origin,
            Origin::Standalone
        );
        assert_eq!(
            store
                .history_for_origin(&Origin::Standalone, &HistoryFilter::default(), 10)
                .unwrap()
                .unwrap()
                .len(),
            1
        );

        // New sends on the migrated database are owned normally.
        let mut owned = sample_entry();
        owned.origin = Origin::Saved {
            node: "node-after".to_owned(),
        };
        let owned_id = store.append_history(&owned).unwrap();
        assert_eq!(
            store.get_history(owned_id).unwrap().unwrap().origin,
            Origin::Saved {
                node: "node-after".to_owned()
            }
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn trims_the_oldest_executions_and_rebuilds_the_index() {
        let _guard = exclusive();
        let dir = test_dir("restbook-sqlcipher-trim-test");
        let store = open_test(dir.join("history.db")).unwrap();

        for index in 0..20 {
            let url = format!("https://example.com/item/{index}");
            store
                .append_history(&entry_named(
                    Method::Get,
                    &url,
                    "item",
                    1_700_000_000_000 + index * 1_000,
                ))
                .unwrap();
        }
        assert_eq!(
            store
                .search_history("example.com", &HistoryFilter::default(), 100)
                .unwrap()
                .len(),
            20
        );

        // 20 rows with a cap of 10 trims to the low-water mark of 9.
        assert_eq!(store.trim_history(10).unwrap(), 11);
        // The indexed query and the LIKE scan must agree: the contentless FTS
        // table was rebuilt, not left pointing at deleted executions.
        assert_eq!(
            store
                .search_history("example.com", &HistoryFilter::default(), 100)
                .unwrap()
                .len(),
            9
        );
        assert_eq!(
            store
                .search_history("example", &HistoryFilter::default(), 100)
                .unwrap()
                .len(),
            9
        );

        // The newest survive and the oldest are gone. Indices 0..=10 were the
        // eleven trimmed; 11..=19 remain.
        assert_eq!(
            store
                .search_history("item/19", &HistoryFilter::default(), 10)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            store
                .search_history("item/11", &HistoryFilter::default(), 10)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            store
                .search_history("item/10", &HistoryFilter::default(), 10)
                .unwrap()
                .len(),
            0
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn filters_narrow_the_index_and_the_scan_alike() {
        let _guard = exclusive();
        let dir = test_dir("restbook-sqlcipher-filter-test");
        let db = dir.join("history.db");
        let base = 1_700_000_000_000;

        {
            let store = open_test(&db).unwrap();
            for (method, status, at_ms) in [
                (Method::Get, 200, base),
                (Method::Post, 404, base + 1_000),
                (Method::Get, 500, base + 2_000),
            ] {
                let mut item = entry_named(
                    method,
                    &format!("https://example.com/{}", method.as_str().to_lowercase()),
                    "thing",
                    at_ms,
                );
                item.response.status = status;
                store.append_history(&item).unwrap();
            }

            // A query of three or more characters takes the FTS path; an empty
            // one takes the LIKE scan. Filters have to work on both.
            let get = HistoryFilter {
                methods: vec!["get".to_owned()],
                ..Default::default()
            };
            assert_eq!(store.search_history("example", &get, 10).unwrap().len(), 2);

            let client_error = HistoryFilter {
                status_classes: vec![4],
                ..Default::default()
            };
            assert_eq!(
                store.search_history("", &client_error, 10).unwrap().len(),
                1
            );

            let recent = HistoryFilter {
                since: Some(chrono::DateTime::from_timestamp_millis(base + 1_000).unwrap()),
                ..Default::default()
            };
            assert_eq!(store.search_history("", &recent, 10).unwrap().len(), 2);
        }

        // The narrowing is storage-side, so it survives a reopen.
        let reopened = open_test(&db).unwrap();
        let get = HistoryFilter {
            methods: vec!["GET".to_owned()],
            ..Default::default()
        };
        assert_eq!(reopened.search_history("", &get, 10).unwrap().len(), 2);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn roundtrips_a_workspace_through_an_encrypted_database() {
        let _guard = exclusive();

        let dir = std::env::temp_dir().join("restbook-sqlcipher-workspace-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let db = dir.join("history.db");
        {
            let store = open_test(&db).unwrap();
            store
                .save_environment(&Environment {
                    name: "dev".to_owned(),
                    vars: vec![Kv::new("host", "localhost")],
                })
                .unwrap();
            store
                .save_collection(&Collection {
                    id: "col-1".to_owned(),
                    name: "Example".to_owned(),
                    items: vec![CollectionItem::Request(SavedRequest {
                        id: "req-1".to_owned(),
                        name: "List".to_owned(),
                        request: RequestDef::new(Method::Get, "https://example.com/things"),
                    })],
                })
                .unwrap();
            store.set_active_environment(Some("dev")).unwrap();
            store.save_globals(&[Kv::new("token", "abc")]).unwrap();
        }

        // Reopen so the roundtrip goes through the encrypted file, not memory.
        let store = open_test(&db).unwrap();

        let environments = store.environments().unwrap();
        assert_eq!(environments.len(), 1);
        assert_eq!(environments[0].name, "dev");
        assert_eq!(environments[0].vars[0].value, "localhost");

        let collections = store.collections().unwrap();
        assert_eq!(collections.len(), 1);
        assert_eq!(collections[0].name, "Example");
        assert!(matches!(
            &collections[0].items[0],
            CollectionItem::Request(request) if request.request.url == "https://example.com/things"
        ));

        assert_eq!(store.active_environment().unwrap().as_deref(), Some("dev"));
        assert_eq!(store.globals().unwrap()[0].key, "token");
    }

    #[test]
    fn roundtrips_drafts_through_an_encrypted_database() {
        let _guard = exclusive();

        let dir = std::env::temp_dir().join("restbook-sqlcipher-drafts-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let db = dir.join("history.db");
        let id = {
            let store = open_test(&db).unwrap();
            let id = store
                .save_draft(&Draft {
                    id: 0,
                    request: RequestDef::new(Method::Post, "https://example.com/drafts"),
                    updated_at: chrono::Utc::now(),
                })
                .unwrap();
            assert!(id > 0);
            id
        };

        // Reopen so the roundtrip goes through the encrypted file, not memory.
        let store = open_test(&db).unwrap();
        let drafts = store.drafts().unwrap();
        assert_eq!(drafts.len(), 1);
        assert_eq!(drafts[0].id, id);
        assert_eq!(drafts[0].request.method, Method::Post);
        assert_eq!(drafts[0].request.url, "https://example.com/drafts");

        store.delete_draft(id).unwrap();
        assert!(store.drafts().unwrap().is_empty());
    }

    /// An entry with a controlled name, method, and timestamp, so the search
    /// tests can order and label their rows without depending on `Utc::now`.
    fn entry_named(method: Method, url: &str, name: &str, at_ms: i64) -> HistoryEntry {
        let mut request = RequestDef::new(method, url);
        request.name = name.to_owned();
        HistoryEntry {
            id: 0,
            url: url.to_owned(),
            request,
            response: ResponseRecord {
                status: 200,
                headers: Vec::new(),
                body: b"ok".to_vec().into(),
                duration_ms: 1,
                received_at: chrono::DateTime::from_timestamp_millis(at_ms).unwrap(),
                final_url: None,
                redirects: Vec::new(),
            },
            origin: Origin::default(),
        }
    }

    fn test_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn search_finds_a_substring_in_the_middle_of_a_url() {
        let _guard = exclusive();
        let dir = test_dir("restbook-sqlcipher-fts-substring-test");

        let store = open_test(dir.join("history.db")).unwrap();
        store
            .append_history(&entry_named(
                Method::Get,
                "https://api.example.com/users/42",
                "fetch user",
                1_700_000_000_000,
            ))
            .unwrap();

        // Neither of these is a prefix of the URL, which is the shape `LIKE
        // '%q%'` supported and the trigram index has to reproduce.
        assert_eq!(
            store
                .search_history("example.co", &HistoryFilter::default(), 10)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            store
                .search_history("users", &HistoryFilter::default(), 10)
                .unwrap()
                .len(),
            1
        );
        // The trigram tokenizer folds ASCII case, which is what `lower() LIKE`
        // did before; a mixed-case query has to keep matching.
        assert_eq!(
            store
                .search_history("USERS", &HistoryFilter::default(), 10)
                .unwrap()
                .len(),
            1
        );
        assert!(store
            .search_history("nothing-here", &HistoryFilter::default(), 10)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn search_keeps_matching_short_queries() {
        let _guard = exclusive();
        let dir = test_dir("restbook-sqlcipher-fts-short-test");

        let store = open_test(dir.join("history.db")).unwrap();
        store
            .append_history(&entry_named(
                Method::Get,
                "https://api.example.com/users/42",
                "fetch user",
                1_700_000_000_000,
            ))
            .unwrap();

        // Two characters cannot be indexed by a trigram tokenizer, so this hit
        // can only come from the `LIKE` fallback.
        let hits = store
            .search_history("us", &HistoryFilter::default(), 10)
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].url, "https://api.example.com/users/42");
    }

    #[test]
    fn search_treats_quotes_in_the_query_as_text() {
        let _guard = exclusive();
        let dir = test_dir("restbook-sqlcipher-fts-quotes-test");

        let store = open_test(dir.join("history.db")).unwrap();
        store
            .append_history(&entry_named(
                Method::Get,
                "https://example.com/q?text=said \"hi\" now",
                "quoted",
                1_700_000_000_000,
            ))
            .unwrap();

        // An unescaped `"` is an FTS5 phrase delimiter; escaped it is just text.
        let hits = store
            .search_history("said \"hi", &HistoryFilter::default(), 10)
            .unwrap();
        assert_eq!(hits.len(), 1);

        // A lone quote is under three characters, so it exercises the fallback.
        assert_eq!(
            store
                .search_history("\"", &HistoryFilter::default(), 10)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn search_matches_method_and_name() {
        let _guard = exclusive();
        let dir = test_dir("restbook-sqlcipher-fts-method-name-test");

        let store = open_test(dir.join("history.db")).unwrap();
        store
            .append_history(&entry_named(
                Method::Patch,
                "https://api.example.com/widgets",
                "Create Widget",
                1_700_000_000_000,
            ))
            .unwrap();

        assert_eq!(
            store
                .search_history("widget", &HistoryFilter::default(), 10)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            store
                .search_history("patc", &HistoryFilter::default(), 10)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn search_survives_reopening_the_database() {
        let _guard = exclusive();
        let dir = test_dir("restbook-sqlcipher-fts-reopen-test");
        let db = dir.join("history.db");

        {
            let store = open_test(&db).unwrap();
            for index in 0..3 {
                store
                    .append_history(&entry_named(
                        Method::Get,
                        &format!("https://api.example.com/items/{index}"),
                        "item",
                        1_700_000_000_000 + index,
                    ))
                    .unwrap();
            }
        }

        // Each open runs the one-time backfill check; a second open must not
        // index the same executions again, or hits would multiply.
        for _ in 0..2 {
            let store = open_test(&db).unwrap();
            assert_eq!(
                store
                    .search_history("example.co", &HistoryFilter::default(), 10)
                    .unwrap()
                    .len(),
                3
            );
        }

        // Hits written after a reopen are indexed on insert, not only by the
        // backfill, and come back newest first.
        let store = open_test(&db).unwrap();
        store
            .append_history(&entry_named(
                Method::Get,
                "https://api.example.com/items/newest",
                "item",
                1_700_000_100_000,
            ))
            .unwrap();
        let hits = store
            .search_history("example.co", &HistoryFilter::default(), 10)
            .unwrap();
        assert_eq!(hits.len(), 4);
        assert_eq!(hits[0].url, "https://api.example.com/items/newest");
    }

    #[test]
    fn history_fts_index_is_available_and_in_sync() {
        let _guard = exclusive();
        let dir = test_dir("restbook-sqlcipher-fts-sync-test");
        let db = dir.join("history.db");

        let store = open_test(&db).unwrap();
        // The bundled SQLCipher has FTS5 with the trigram tokenizer. If this
        // ever fails, the store still works but search is back on the scan.
        assert!(store.fts, "FTS5 trigram is unavailable in this build");

        for index in 0..3 {
            store
                .append_history(&entry_named(
                    Method::Get,
                    &format!("https://api.example.com/items/{index}"),
                    "item",
                    1_700_000_000_000 + index,
                ))
                .unwrap();
        }

        // One index row per execution, keyed by the execution id.
        let conn = store.conn.lock().unwrap();
        let executions: i64 = conn
            .query_row("SELECT count(*) FROM executions", [], |row| row.get(0))
            .unwrap();
        let indexed: i64 = conn
            .query_row("SELECT count(*) FROM history_fts", [], |row| row.get(0))
            .unwrap();
        assert_eq!(indexed, executions);
    }
}
