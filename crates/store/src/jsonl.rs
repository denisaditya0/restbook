//! Append-only JSONL history.
//!
//! **Not encrypted.** This exists so the engine can be exercised end to end
//! before the SQLCipher store lands; the desktop app must not use it.

use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use restbook_core::{Error, HistoryEntry, HistoryHit, Result, Store};

pub struct JsonlStore {
    path: PathBuf,
    write_lock: Mutex<()>,
}

impl JsonlStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }
        OpenOptions::new().create(true).append(true).open(&path)?;
        Ok(Self {
            path,
            write_lock: Mutex::new(()),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn read_all(&self) -> Result<Vec<HistoryEntry>> {
        let file = match File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };

        let mut entries = Vec::new();
        for line in BufReader::new(file).lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            entries.push(serde_json::from_str(&line)?);
        }
        Ok(entries)
    }
}

impl Store for JsonlStore {
    fn append_history(&self, entry: &HistoryEntry) -> Result<i64> {
        let _guard = self
            .write_lock
            .lock()
            .map_err(|_| Error::Store("history lock poisoned".to_owned()))?;

        let next_id = self
            .read_all()?
            .last()
            .map(|entry| entry.id + 1)
            .unwrap_or(1);

        let mut stored = entry.clone();
        stored.id = next_id;

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        writeln!(file, "{}", serde_json::to_string(&stored)?)?;
        file.flush()?;
        Ok(next_id)
    }

    fn search_history(&self, query: &str, limit: u32) -> Result<Vec<HistoryHit>> {
        Ok(crate::matching(&self.read_all()?, query, limit))
    }
}
