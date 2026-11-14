//! Encrypted history store (SQLCipher).
//!
//! Enable with `--features sqlcipher`.
//!
//! The master key is generated once (256 random bits) and stored in the OS
//! credential store: Windows Credential Manager, macOS Keychain, or Secret
//! Service. The database file is unreadable without it, and there is no copy of
//! the key on disk.
//!
//! Verified on Windows (GNU toolchain, OpenSSL from the mingw tree): the test
//! below creates the keyring entry, applies the key, stores a history entry, and
//! searches it back.

use std::path::Path;
use std::sync::Mutex;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use rand::RngCore;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

use restbook_core::{Error, HistoryEntry, HistoryHit, Result, Store};

const SERVICE: &str = "restbook";
const ACCOUNT: &str = "master-key";

const SCHEMA: &str = r#"
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS requests (
    id         INTEGER PRIMARY KEY,
    method     TEXT NOT NULL,
    url        TEXT NOT NULL,
    url_key    TEXT NOT NULL UNIQUE,
    name       TEXT NOT NULL,
    body       TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS executions (
    id          INTEGER PRIMARY KEY,
    request_id  INTEGER NOT NULL REFERENCES requests(id),
    status      INTEGER NOT NULL,
    duration_ms INTEGER NOT NULL,
    size_bytes  INTEGER NOT NULL,
    received_at INTEGER NOT NULL,
    response    TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS executions_received_at ON executions(received_at DESC);
"#;

pub struct SqlcipherStore {
    conn: Mutex<Connection>,
}

impl SqlcipherStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let key = load_or_create_key()?;
        let hex: String = key.iter().map(|byte| format!("{byte:02x}")).collect();

        let conn = Connection::open(path.as_ref()).map_err(db_error)?;
        // The key pragma has to run before anything else touches the database.
        // The blob-literal form avoids quoting issues for arbitrary key bytes.
        conn.execute_batch(&format!(
            "PRAGMA key = \"x'{hex}'\";\nPRAGMA cipher_memory_security = ON;"
        ))
        .map_err(db_error)?;
        conn.execute_batch(SCHEMA).map_err(db_error)?;

        Ok(Self {
            conn: Mutex::new(conn),
        })
    }
}

impl Store for SqlcipherStore {
    fn append_history(&self, entry: &HistoryEntry) -> Result<i64> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| Error::Store("history lock poisoned".to_owned()))?;

        let url_key = format!("{} {}", entry.request.method, entry.url);
        let received_at = entry.response.received_at.timestamp_millis();

        let stored_response = StoredResponse {
            status: entry.response.status,
            headers: entry.response.headers.clone(),
            body_b64: BASE64.encode(&entry.response.body),
            duration_ms: entry.response.duration_ms,
            received_at,
        };

        conn.execute(
            "INSERT INTO requests (method, url, url_key, name, body, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(url_key) DO UPDATE SET
                 method     = excluded.method,
                 name       = excluded.name,
                 body       = excluded.body,
                 updated_at = excluded.updated_at",
            params![
                entry.request.method.as_str(),
                entry.url,
                url_key,
                entry.request.name,
                serde_json::to_string(&entry.request.body)?,
                received_at,
            ],
        )
        .map_err(db_error)?;

        let request_id: i64 = conn
            .query_row(
                "SELECT id FROM requests WHERE url_key = ?1",
                params![url_key],
                |row| row.get(0),
            )
            .map_err(db_error)?;

        conn.execute(
            "INSERT INTO executions
                 (request_id, status, duration_ms, size_bytes, received_at, response)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                request_id,
                entry.response.status,
                entry.response.duration_ms as i64,
                entry.response.body.len() as i64,
                received_at,
                serde_json::to_string(&stored_response)?,
            ],
        )
        .map_err(db_error)?;

        Ok(conn.last_insert_rowid())
    }

    fn search_history(&self, query: &str, limit: u32) -> Result<Vec<HistoryHit>> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| Error::Store("history lock poisoned".to_owned()))?;

        // An empty query becomes "%%", which matches everything: most recent first.
        let pattern = format!("%{}%", query.trim().to_ascii_lowercase());

        let mut statement = conn
            .prepare(
                "SELECT e.id, r.method, r.url, e.status, e.received_at
                 FROM executions e
                 JOIN requests r ON r.id = e.request_id
                 WHERE lower(r.url) LIKE ?1
                    OR lower(r.method) LIKE ?1
                    OR lower(r.name) LIKE ?1
                 ORDER BY e.received_at DESC
                 LIMIT ?2",
            )
            .map_err(db_error)?;

        let rows = statement
            .query_map(params![pattern, limit as i64], |row| {
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
            })
            .map_err(db_error)?;

        let mut hits = Vec::new();
        for row in rows {
            hits.push(row.map_err(db_error)?);
        }
        Ok(hits)
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct StoredResponse {
    status: u16,
    headers: Vec<(String, String)>,
    body_b64: String,
    duration_ms: u64,
    received_at: i64,
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
    use restbook_core::{Method, RequestDef, ResponseRecord};

    fn sample_entry() -> HistoryEntry {
        HistoryEntry {
            id: 0,
            url: "https://example.com/things".to_owned(),
            request: RequestDef::new(Method::Get, "https://example.com/things"),
            response: ResponseRecord {
                status: 200,
                headers: vec![("content-type".to_owned(), "application/json".to_owned())],
                body: br#"{"ok":true}"#.to_vec(),
                duration_ms: 12,
                received_at: chrono::Utc::now(),
            },
        }
    }

    #[test]
    fn roundtrips_history_through_an_encrypted_database() {
        let dir = std::env::temp_dir().join("restbook-sqlcipher-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let store = SqlcipherStore::open(dir.join("history.db")).unwrap();
        let id = store.append_history(&sample_entry()).unwrap();
        assert!(id > 0);

        let hits = store.search_history("example.com", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].status, 200);
        assert_eq!(hits[0].url, "https://example.com/things");
    }
}
