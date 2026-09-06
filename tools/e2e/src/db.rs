//! Read-only observation of the durable state DB. Every call opens the
//! file read-only and closes it again, so the harness never holds a
//! handle that could interfere with the daemon's WAL checkpoints.
//! Transient contention reads as `None`; poll loops just retry.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};

#[derive(Clone, Debug)]
pub struct StateDb {
    pub path: PathBuf,
}

impl StateDb {
    pub fn at(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
        }
    }

    pub fn exists(&self) -> bool {
        self.path.is_file()
    }

    fn open(&self) -> Option<Connection> {
        let connection = Connection::open_with_flags(
            &self.path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .ok()?;
        connection
            .busy_timeout(std::time::Duration::from_millis(500))
            .ok()?;
        Some(connection)
    }

    /// One integer scalar, or `None` on any error (missing file, busy).
    pub fn scalar_i64(&self, sql: &str) -> Option<i64> {
        let connection = self.open()?;
        connection
            .query_row(sql, [], |row| row.get::<_, i64>(0))
            .ok()
    }

    pub fn scalar_text(&self, sql: &str) -> Option<String> {
        let connection = self.open()?;
        connection
            .query_row(sql, [], |row| row.get::<_, String>(0))
            .ok()
    }

    /// Rows rendered as pipe-separated text, for diagnostics dumps.
    pub fn rows(&self, sql: &str) -> Vec<String> {
        let Some(connection) = self.open() else {
            return Vec::new();
        };
        let Ok(mut statement) = connection.prepare(sql) else {
            return Vec::new();
        };
        let column_count = statement.column_count();
        let Ok(rows) = statement.query_map([], |row| {
            let mut cells = Vec::with_capacity(column_count);
            for index in 0..column_count {
                let cell = match row.get_ref(index) {
                    Ok(rusqlite::types::ValueRef::Null) => "NULL".to_string(),
                    Ok(rusqlite::types::ValueRef::Integer(value)) => value.to_string(),
                    Ok(rusqlite::types::ValueRef::Real(value)) => value.to_string(),
                    Ok(rusqlite::types::ValueRef::Text(bytes)) => {
                        String::from_utf8_lossy(bytes).into_owned()
                    }
                    Ok(rusqlite::types::ValueRef::Blob(bytes)) => {
                        format!("<{} bytes>", bytes.len())
                    }
                    Err(_) => "?".to_string(),
                };
                cells.push(cell);
            }
            Ok(cells.join(" | "))
        }) else {
            return Vec::new();
        };
        rows.flatten().collect()
    }

    /// Workable rows: pending and leased. A row held behind a decision
    /// is parked, not queued; `held_intents` counts those.
    pub fn pending_intents(&self) -> Option<i64> {
        self.scalar_i64("SELECT COUNT(*) FROM queue_intents WHERE state IN ('pending', 'leased');")
    }

    pub fn held_intents(&self) -> Option<i64> {
        self.scalar_i64("SELECT COUNT(*) FROM queue_intents WHERE state = 'held';")
    }

    pub fn failed_intents(&self) -> Option<i64> {
        self.scalar_i64("SELECT COUNT(*) FROM failed_intents;")
    }

    /// Monotonic count of every intent ever enqueued (the AUTOINCREMENT
    /// sequence), so a burst that was captured and drained between two
    /// polls is still observable.
    pub fn enqueue_high_water(&self) -> Option<i64> {
        self.scalar_i64(
            "SELECT COALESCE((SELECT seq FROM sqlite_sequence WHERE name = 'queue_intents'), 0);",
        )
    }

    pub fn queue_drained(&self) -> bool {
        self.pending_intents() == Some(0)
    }

    pub fn sync_index_count(&self) -> Option<i64> {
        self.scalar_i64("SELECT COUNT(*) FROM sync_index;")
    }

    pub fn tombstone_count(&self) -> Option<i64> {
        self.scalar_i64("SELECT COUNT(*) FROM tombstones;")
    }
}
