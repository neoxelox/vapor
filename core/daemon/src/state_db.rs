use std::error::Error;
use std::fmt;
use std::fs;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use vapor_shared::{constants, runtime_paths};

use crate::event_intents::PendingIntentKind;
use crate::retry::{RetryDecision, RetryFailureKind, RetryPolicy};

const CURRENT_SCHEMA_VERSION: i64 = 2;
const STATE_PENDING: &str = "pending";
const STATE_LEASED: &str = "leased";

#[derive(Debug)]
pub enum StateDbError {
    Io(std::io::Error),
    Sql(rusqlite::Error),
    SchemaVersionMismatch { found: i64, expected: i64 },
    MissingSchemaVersion,
    InvalidSchemaVersion(i64),
    InvalidTimestampMillis(i64),
    InvalidStateValue(String),
    InvalidIntentKind(String),
    InvalidIntentState(String),
    TimeBeforeUnixEpoch,
    MissingIntentRecord(i64),
}

impl fmt::Display for StateDbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "I/O error: {error}"),
            Self::Sql(error) => write!(f, "SQLite error: {error}"),
            Self::SchemaVersionMismatch { found, expected } => {
                write!(f, "unsupported schema version {found}; expected {expected}")
            }
            Self::MissingSchemaVersion => write!(f, "missing schema version row"),
            Self::InvalidSchemaVersion(version) => {
                write!(f, "invalid schema version value {version}")
            }
            Self::InvalidTimestampMillis(millis) => {
                write!(f, "invalid timestamp millis value {millis}")
            }
            Self::InvalidStateValue(message) => write!(f, "invalid state value: {message}"),
            Self::InvalidIntentKind(kind) => write!(f, "invalid intent kind '{kind}'"),
            Self::InvalidIntentState(state) => write!(f, "invalid intent state '{state}'"),
            Self::TimeBeforeUnixEpoch => write!(f, "time before UNIX epoch is unsupported"),
            Self::MissingIntentRecord(id) => write!(f, "missing durable intent record {id}"),
        }
    }
}

impl Error for StateDbError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Sql(error) => Some(error),
            _ => None,
        }
    }
}

impl From<std::io::Error> for StateDbError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<rusqlite::Error> for StateDbError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sql(error)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DurableIntentRecord {
    pub id: i64,
    pub path: PathBuf,
    pub kind: PendingIntentKind,
    pub enqueued_at: SystemTime,
    pub available_at: SystemTime,
    pub leased_at: Option<SystemTime>,
    pub attempt_count: u32,
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StateEntry {
    pub key: String,
    pub value: String,
    pub updated_at: SystemTime,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScheduledRetryRecord {
    pub intent: DurableIntentRecord,
    pub decision: RetryDecision,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DurableFailedIntentRecord {
    pub id: i64,
    pub path: PathBuf,
    pub kind: PendingIntentKind,
    pub failure_kind: RetryFailureKind,
    pub enqueued_at: SystemTime,
    pub failed_at: SystemTime,
    pub attempt_count: u32,
    pub last_error: String,
}

#[derive(Debug)]
pub struct DurableStateDb {
    path: PathBuf,
    connection: Connection,
}

impl DurableStateDb {
    pub fn open_default() -> Result<Self, StateDbError> {
        Self::open(runtime_paths::sqlite_database_path())
    }

    pub fn open(path: impl AsRef<Path>) -> Result<Self, StateDbError> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let mut connection = Connection::open(&path)?;
        configure_connection(&connection)?;
        migrate_schema(&mut connection)?;

        Ok(Self { path, connection })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn schema_version(&self) -> Result<i64, StateDbError> {
        read_schema_version(&self.connection)?.ok_or(StateDbError::MissingSchemaVersion)
    }

    pub fn queue_depth(&self) -> Result<usize, StateDbError> {
        count_intents(&self.connection, None)
    }

    pub fn pending_depth(&self) -> Result<usize, StateDbError> {
        count_intents(&self.connection, Some(STATE_PENDING))
    }

    pub fn leased_depth(&self) -> Result<usize, StateDbError> {
        count_intents(&self.connection, Some(STATE_LEASED))
    }

    pub fn failed_depth(&self) -> Result<usize, StateDbError> {
        let count =
            self.connection
                .query_row("SELECT COUNT(*) FROM failed_intents", [], |row| {
                    row.get::<_, i64>(0)
                })?;
        Ok(count as usize)
    }

    pub fn enqueue_intent(
        &mut self,
        path: &Path,
        kind: PendingIntentKind,
        now: SystemTime,
    ) -> Result<DurableIntentRecord, StateDbError> {
        self.enqueue_intent_with_available_at(path, kind, now, now)
    }

    pub fn enqueue_startup_reconcile_intent(
        &mut self,
        path: &Path,
        now: SystemTime,
    ) -> Result<DurableIntentRecord, StateDbError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let path_bytes = path_to_bytes(path);
        let existing = transaction
            .query_row(
                "SELECT id, state
                 FROM queue_intents
                 WHERE path_bytes = ? AND kind = ? AND state IN (?, ?)
                 ORDER BY available_at_ms ASC, id ASC
                 LIMIT 1",
                params![
                    path_bytes,
                    intent_kind_label(PendingIntentKind::ReconcileSubtree),
                    STATE_PENDING,
                    STATE_LEASED
                ],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;

        let intent = if let Some((existing_id, state)) = existing {
            if state == STATE_PENDING {
                transaction.execute(
                    "UPDATE queue_intents
                     SET available_at_ms = ?, last_error = NULL
                     WHERE id = ? AND state = ?",
                    params![0_i64, existing_id, STATE_PENDING],
                )?;
            }
            fetch_intent(&transaction, existing_id)?
                .ok_or(StateDbError::MissingIntentRecord(existing_id))?
        } else {
            let id = insert_intent(
                &transaction,
                path,
                PendingIntentKind::ReconcileSubtree,
                now,
                UNIX_EPOCH,
            )?;
            fetch_intent(&transaction, id)?.ok_or(StateDbError::MissingIntentRecord(id))?
        };

        transaction.commit()?;
        Ok(intent)
    }

    pub fn intent_record(&self, id: i64) -> Result<Option<DurableIntentRecord>, StateDbError> {
        fetch_intent(&self.connection, id)
    }

    pub fn failed_record(
        &self,
        id: i64,
    ) -> Result<Option<DurableFailedIntentRecord>, StateDbError> {
        fetch_failed_intent(&self.connection, id)
    }

    pub fn lease_next_ready(
        &mut self,
        now: SystemTime,
    ) -> Result<Option<DurableIntentRecord>, StateDbError> {
        let now_ms = system_time_to_millis(now)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let intent_id = transaction
            .query_row(
                "SELECT id
                 FROM queue_intents
                 WHERE state = ? AND available_at_ms <= ?
                 ORDER BY available_at_ms ASC, id ASC
                 LIMIT 1",
                params![STATE_PENDING, now_ms],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;

        let Some(intent_id) = intent_id else {
            transaction.commit()?;
            return Ok(None);
        };

        transaction.execute(
            "UPDATE queue_intents
             SET state = ?, leased_at_ms = ?, attempt_count = attempt_count + 1
             WHERE id = ? AND state = ?",
            params![STATE_LEASED, now_ms, intent_id, STATE_PENDING],
        )?;

        let leased_intent = fetch_intent(&transaction, intent_id)?
            .ok_or(StateDbError::MissingIntentRecord(intent_id))?;
        transaction.commit()?;
        Ok(Some(leased_intent))
    }

    pub fn complete_leased(&mut self, id: i64) -> Result<bool, StateDbError> {
        let changed = self.connection.execute(
            "DELETE FROM queue_intents WHERE id = ? AND state = ?",
            params![id, STATE_LEASED],
        )?;
        Ok(changed > 0)
    }

    pub fn requeue_leased(
        &mut self,
        id: i64,
        available_at: SystemTime,
        last_error: Option<&str>,
    ) -> Result<bool, StateDbError> {
        let available_at_ms = system_time_to_millis(available_at)?;
        let changed = self.connection.execute(
            "UPDATE queue_intents
             SET state = ?, available_at_ms = ?, leased_at_ms = NULL, last_error = ?
             WHERE id = ? AND state = ?",
            params![STATE_PENDING, available_at_ms, last_error, id, STATE_LEASED],
        )?;
        Ok(changed > 0)
    }

    pub fn schedule_retry(
        &mut self,
        id: i64,
        failure_kind: RetryFailureKind,
        last_error: &str,
        now: SystemTime,
    ) -> Result<Option<ScheduledRetryRecord>, StateDbError> {
        let leased_intent = self
            .intent_record(id)?
            .ok_or(StateDbError::MissingIntentRecord(id))?;
        let decision = RetryPolicy::default().decide(
            leased_intent.id,
            leased_intent.attempt_count,
            failure_kind,
            now,
        );
        if !decision.retryable {
            return Err(StateDbError::InvalidIntentState(format!(
                "non-retryable '{}' failure must be finalized separately",
                failure_kind.label()
            )));
        }

        let available_at = decision
            .available_at
            .ok_or(StateDbError::InvalidIntentState(
                "retryable decision missing available_at".to_string(),
            ))?;
        if !self.requeue_leased(id, available_at, Some(last_error))? {
            return Err(StateDbError::InvalidIntentState(format!(
                "intent {id} is not currently leased"
            )));
        }
        if let Some(slowdown_until) = decision.slowdown_until {
            let persisted_slowdown_until = self
                .retry_slowdown_until()?
                .map(|existing| existing.max(slowdown_until))
                .unwrap_or(slowdown_until);
            self.set_state(
                constants::state::RETRY_SLOWDOWN_UNTIL_KEY,
                &system_time_to_millis(persisted_slowdown_until)?.to_string(),
                now,
            )?;
        }

        let intent = self
            .intent_record(id)?
            .ok_or(StateDbError::MissingIntentRecord(id))?;
        Ok(Some(ScheduledRetryRecord { intent, decision }))
    }

    pub fn finalize_leased_failure(
        &mut self,
        id: i64,
        failure_kind: RetryFailureKind,
        last_error: &str,
        failed_at: SystemTime,
    ) -> Result<DurableFailedIntentRecord, StateDbError> {
        let Some(failure_label) = terminal_failure_label(failure_kind) else {
            return Err(StateDbError::InvalidIntentState(format!(
                "retryable '{}' failures must be requeued instead of finalized",
                failure_kind.label()
            )));
        };
        let failed_at_ms = system_time_to_millis(failed_at)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let inserted = transaction.execute(
            "INSERT INTO failed_intents (
                 id,
                 path_bytes,
                 kind,
                 failure_kind,
                 enqueued_at_ms,
                 failed_at_ms,
                 attempt_count,
                 last_error
             )
             SELECT
                 id,
                 path_bytes,
                 kind,
                 ?,
                 enqueued_at_ms,
                 ?,
                 attempt_count,
                 ?
             FROM queue_intents
             WHERE id = ? AND state = ?",
            params![failure_label, failed_at_ms, last_error, id, STATE_LEASED],
        )?;
        if inserted == 0 {
            return Err(StateDbError::InvalidIntentState(format!(
                "intent {id} is not currently leased"
            )));
        }
        transaction.execute(
            "DELETE FROM queue_intents WHERE id = ? AND state = ?",
            params![id, STATE_LEASED],
        )?;
        let failed_record =
            fetch_failed_intent(&transaction, id)?.ok_or(StateDbError::MissingIntentRecord(id))?;
        transaction.commit()?;
        Ok(failed_record)
    }

    pub fn recover_leased(&mut self, now: SystemTime) -> Result<usize, StateDbError> {
        let now_ms = system_time_to_millis(now)?;
        let changed = self.connection.execute(
            "UPDATE queue_intents
             SET state = ?, available_at_ms = ?, leased_at_ms = NULL
             WHERE state = ?",
            params![STATE_PENDING, now_ms, STATE_LEASED],
        )?;
        Ok(changed)
    }

    pub fn take_active_retry_slowdown_until(
        &mut self,
        now: SystemTime,
    ) -> Result<Option<SystemTime>, StateDbError> {
        let Some(slowdown_until) = self.retry_slowdown_until()? else {
            return Ok(None);
        };
        if slowdown_until > now {
            return Ok(Some(slowdown_until));
        }

        self.delete_state(constants::state::RETRY_SLOWDOWN_UNTIL_KEY)?;
        Ok(None)
    }

    pub fn set_state(
        &mut self,
        key: &str,
        value: &str,
        updated_at: SystemTime,
    ) -> Result<StateEntry, StateDbError> {
        let updated_at_ms = system_time_to_millis(updated_at)?;
        self.connection.execute(
            "INSERT INTO state_entries (key, value, updated_at_ms)
             VALUES (?, ?, ?)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at_ms = excluded.updated_at_ms",
            params![key, value, updated_at_ms],
        )?;
        Ok(StateEntry {
            key: key.to_string(),
            value: value.to_string(),
            updated_at,
        })
    }

    pub fn state(&self, key: &str) -> Result<Option<StateEntry>, StateDbError> {
        let raw_entry = self
            .connection
            .query_row(
                "SELECT key, value, updated_at_ms FROM state_entries WHERE key = ?",
                params![key],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                },
            )
            .optional()?;

        raw_entry
            .map(|(entry_key, value, updated_at_ms)| {
                Ok(StateEntry {
                    key: entry_key,
                    value,
                    updated_at: millis_to_system_time(updated_at_ms)?,
                })
            })
            .transpose()
    }

    pub fn retry_slowdown_until(&self) -> Result<Option<SystemTime>, StateDbError> {
        let Some(entry) = self.state(constants::state::RETRY_SLOWDOWN_UNTIL_KEY)? else {
            return Ok(None);
        };
        let millis = entry.value.parse::<i64>().map_err(|_| {
            StateDbError::InvalidStateValue(format!(
                "{}={} is not a millis timestamp",
                constants::state::RETRY_SLOWDOWN_UNTIL_KEY,
                entry.value
            ))
        })?;
        Ok(Some(millis_to_system_time(millis)?))
    }

    pub fn delete_state(&mut self, key: &str) -> Result<bool, StateDbError> {
        let changed = self
            .connection
            .execute("DELETE FROM state_entries WHERE key = ?", params![key])?;
        Ok(changed > 0)
    }

    fn enqueue_intent_with_available_at(
        &mut self,
        path: &Path,
        kind: PendingIntentKind,
        enqueued_at: SystemTime,
        available_at: SystemTime,
    ) -> Result<DurableIntentRecord, StateDbError> {
        let id = insert_intent(&self.connection, path, kind, enqueued_at, available_at)?;
        self.intent_record(id)?
            .ok_or(StateDbError::MissingIntentRecord(id))
    }
}

fn insert_intent(
    connection: &Connection,
    path: &Path,
    kind: PendingIntentKind,
    enqueued_at: SystemTime,
    available_at: SystemTime,
) -> Result<i64, StateDbError> {
    let enqueued_at_ms = system_time_to_millis(enqueued_at)?;
    let available_at_ms = system_time_to_millis(available_at)?;
    connection.execute(
        "INSERT INTO queue_intents (
            path_bytes,
            kind,
            state,
            enqueued_at_ms,
            available_at_ms,
            leased_at_ms,
            attempt_count,
            last_error
        ) VALUES (?, ?, ?, ?, ?, NULL, 0, NULL)",
        params![
            path_to_bytes(path),
            intent_kind_label(kind),
            STATE_PENDING,
            enqueued_at_ms,
            available_at_ms
        ],
    )?;
    Ok(connection.last_insert_rowid())
}

fn configure_connection(connection: &Connection) -> Result<(), StateDbError> {
    connection.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = FULL;
         PRAGMA foreign_keys = ON;
         PRAGMA busy_timeout = 5000;",
    )?;
    Ok(())
}

fn migrate_schema(connection: &mut Connection) -> Result<(), StateDbError> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    transaction.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_meta (
             singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
             schema_version INTEGER NOT NULL CHECK(schema_version > 0)
         );
         CREATE TABLE IF NOT EXISTS queue_intents (
             id INTEGER PRIMARY KEY AUTOINCREMENT,
             path_bytes BLOB NOT NULL,
             kind TEXT NOT NULL CHECK(kind IN ('upload', 'delete', 'rename', 'reconcile_subtree')),
             state TEXT NOT NULL CHECK(state IN ('pending', 'leased')),
             enqueued_at_ms INTEGER NOT NULL,
             available_at_ms INTEGER NOT NULL,
             leased_at_ms INTEGER,
             attempt_count INTEGER NOT NULL DEFAULT 0 CHECK(attempt_count >= 0),
             last_error TEXT
         );
         CREATE INDEX IF NOT EXISTS idx_queue_intents_ready
             ON queue_intents(state, available_at_ms, id);
         CREATE TABLE IF NOT EXISTS failed_intents (
             id INTEGER PRIMARY KEY,
             path_bytes BLOB NOT NULL,
             kind TEXT NOT NULL CHECK(kind IN ('upload', 'delete', 'rename', 'reconcile_subtree')),
             failure_kind TEXT NOT NULL CHECK(failure_kind IN ('authentication', 'permanent')),
             enqueued_at_ms INTEGER NOT NULL,
             failed_at_ms INTEGER NOT NULL,
             attempt_count INTEGER NOT NULL CHECK(attempt_count >= 0),
             last_error TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS state_entries (
             key TEXT PRIMARY KEY,
             value TEXT NOT NULL,
             updated_at_ms INTEGER NOT NULL
         );",
    )?;

    match read_schema_version(&transaction)? {
        Some(CURRENT_SCHEMA_VERSION) => {}
        Some(1) => {
            transaction.execute(
                "UPDATE schema_meta SET schema_version = ? WHERE singleton = 1",
                params![CURRENT_SCHEMA_VERSION],
            )?;
        }
        Some(found) => {
            return Err(StateDbError::SchemaVersionMismatch {
                found,
                expected: CURRENT_SCHEMA_VERSION,
            });
        }
        None => {
            transaction.execute(
                "INSERT INTO schema_meta (singleton, schema_version) VALUES (1, ?)",
                params![CURRENT_SCHEMA_VERSION],
            )?;
        }
    }

    transaction.commit()?;
    Ok(())
}

fn read_schema_version(connection: &Connection) -> Result<Option<i64>, StateDbError> {
    let version = connection
        .query_row(
            "SELECT schema_version FROM schema_meta WHERE singleton = 1",
            [],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;

    if let Some(version) = version
        && version <= 0
    {
        return Err(StateDbError::InvalidSchemaVersion(version));
    }

    Ok(version)
}

fn count_intents(connection: &Connection, state: Option<&str>) -> Result<usize, StateDbError> {
    let count = match state {
        Some(state) => connection.query_row(
            "SELECT COUNT(*) FROM queue_intents WHERE state = ?",
            params![state],
            |row| row.get::<_, i64>(0),
        )?,
        None => connection.query_row("SELECT COUNT(*) FROM queue_intents", [], |row| {
            row.get::<_, i64>(0)
        })?,
    };
    Ok(count as usize)
}

fn fetch_intent(
    connection: &Connection,
    id: i64,
) -> Result<Option<DurableIntentRecord>, StateDbError> {
    let raw_intent = connection
        .query_row(
            "SELECT id, path_bytes, kind, enqueued_at_ms, available_at_ms, leased_at_ms, attempt_count, last_error
             FROM queue_intents
             WHERE id = ?",
            params![id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Vec<u8>>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, Option<i64>>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, Option<String>>(7)?,
                ))
            },
        )
        .optional()?;

    raw_intent
        .map(
            |(
                row_id,
                path_bytes,
                kind,
                enqueued_at_ms,
                available_at_ms,
                leased_at_ms,
                attempt_count,
                last_error,
            )| {
                Ok(DurableIntentRecord {
                    id: row_id,
                    path: path_from_bytes(path_bytes),
                    kind: intent_kind_from_label(&kind)?,
                    enqueued_at: millis_to_system_time(enqueued_at_ms)?,
                    available_at: millis_to_system_time(available_at_ms)?,
                    leased_at: leased_at_ms.map(millis_to_system_time).transpose()?,
                    attempt_count: attempt_count as u32,
                    last_error,
                })
            },
        )
        .transpose()
}

fn fetch_failed_intent(
    connection: &Connection,
    id: i64,
) -> Result<Option<DurableFailedIntentRecord>, StateDbError> {
    let raw_intent = connection
        .query_row(
            "SELECT id, path_bytes, kind, failure_kind, enqueued_at_ms, failed_at_ms, attempt_count, last_error
             FROM failed_intents
             WHERE id = ?",
            params![id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Vec<u8>>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, String>(7)?,
                ))
            },
        )
        .optional()?;

    raw_intent
        .map(
            |(
                row_id,
                path_bytes,
                kind,
                failure_kind,
                enqueued_at_ms,
                failed_at_ms,
                attempt_count,
                last_error,
            )| {
                Ok(DurableFailedIntentRecord {
                    id: row_id,
                    path: path_from_bytes(path_bytes),
                    kind: intent_kind_from_label(&kind)?,
                    failure_kind: terminal_failure_from_label(&failure_kind)?,
                    enqueued_at: millis_to_system_time(enqueued_at_ms)?,
                    failed_at: millis_to_system_time(failed_at_ms)?,
                    attempt_count: attempt_count as u32,
                    last_error,
                })
            },
        )
        .transpose()
}

fn path_to_bytes(path: &Path) -> Vec<u8> {
    path.as_os_str().as_bytes().to_vec()
}

fn path_from_bytes(bytes: Vec<u8>) -> PathBuf {
    PathBuf::from(std::ffi::OsString::from_vec(bytes))
}

fn system_time_to_millis(time: SystemTime) -> Result<i64, StateDbError> {
    let duration = time
        .duration_since(UNIX_EPOCH)
        .map_err(|_| StateDbError::TimeBeforeUnixEpoch)?;
    Ok(duration.as_millis() as i64)
}

fn millis_to_system_time(millis: i64) -> Result<SystemTime, StateDbError> {
    if millis < 0 {
        return Err(StateDbError::InvalidTimestampMillis(millis));
    }
    Ok(UNIX_EPOCH + Duration::from_millis(millis as u64))
}

fn intent_kind_label(kind: PendingIntentKind) -> &'static str {
    match kind {
        PendingIntentKind::Upload => "upload",
        PendingIntentKind::Delete => "delete",
        PendingIntentKind::Rename => "rename",
        PendingIntentKind::ReconcileSubtree => "reconcile_subtree",
    }
}

fn intent_kind_from_label(label: &str) -> Result<PendingIntentKind, StateDbError> {
    match label {
        "upload" => Ok(PendingIntentKind::Upload),
        "delete" => Ok(PendingIntentKind::Delete),
        "rename" => Ok(PendingIntentKind::Rename),
        "reconcile_subtree" => Ok(PendingIntentKind::ReconcileSubtree),
        other => Err(StateDbError::InvalidIntentKind(other.to_string())),
    }
}

fn terminal_failure_label(failure_kind: RetryFailureKind) -> Option<&'static str> {
    match failure_kind {
        RetryFailureKind::Authentication => Some("authentication"),
        RetryFailureKind::Permanent => Some("permanent"),
        RetryFailureKind::Transient | RetryFailureKind::RateLimited { .. } => None,
    }
}

fn terminal_failure_from_label(label: &str) -> Result<RetryFailureKind, StateDbError> {
    match label {
        "authentication" => Ok(RetryFailureKind::Authentication),
        "permanent" => Ok(RetryFailureKind::Permanent),
        other => Err(StateDbError::InvalidIntentState(format!(
            "invalid terminal failure kind '{other}'"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::retry::RetryFailureKind;
    use tempfile::TempDir;

    #[test]
    fn opening_database_creates_schema_and_parent_directory() {
        let temp_dir = TempDir::new().expect("temp dir");
        let database_path = temp_dir.path().join("state/vapor.sqlite");

        let database = DurableStateDb::open(&database_path).expect("open durable state db");

        assert_eq!(database.path(), database_path.as_path());
        assert!(database_path.exists());
        assert_eq!(
            database.schema_version().expect("schema version"),
            CURRENT_SCHEMA_VERSION
        );
        assert_eq!(database.queue_depth().expect("queue depth"), 0);
    }

    #[test]
    fn enqueue_and_complete_intent_round_trip() {
        let temp_dir = TempDir::new().expect("temp dir");
        let database_path = temp_dir.path().join("state/vapor.sqlite");
        let mut database = DurableStateDb::open(&database_path).expect("open durable state db");
        let path = PathBuf::from("/tmp/vapor-root/project/file.txt");

        let queued = database
            .enqueue_intent(&path, PendingIntentKind::Upload, timestamp_ms(100))
            .expect("enqueue intent");
        assert_eq!(queued.path, path);
        assert_eq!(queued.kind, PendingIntentKind::Upload);
        assert_eq!(queued.attempt_count, 0);
        assert_eq!(database.pending_depth().expect("pending depth"), 1);

        let leased = database
            .lease_next_ready(timestamp_ms(100))
            .expect("lease next ready")
            .expect("leased record");
        assert_eq!(leased.id, queued.id);
        assert_eq!(leased.attempt_count, 1);
        assert!(leased.leased_at.is_some());
        assert_eq!(database.pending_depth().expect("pending depth"), 0);
        assert_eq!(database.leased_depth().expect("leased depth"), 1);

        assert!(
            database
                .complete_leased(leased.id)
                .expect("complete leased")
        );
        assert_eq!(database.queue_depth().expect("queue depth"), 0);
    }

    #[test]
    fn startup_reconcile_is_prioritized_ahead_of_existing_pending_work() {
        let temp_dir = TempDir::new().expect("temp dir");
        let database_path = temp_dir.path().join("state/vapor.sqlite");
        let mut database = DurableStateDb::open(&database_path).expect("open durable state db");
        let watch_root = PathBuf::from("/tmp/vapor-root");

        database
            .enqueue_intent(
                &watch_root.join("src/main.rs"),
                PendingIntentKind::Upload,
                timestamp_ms(100),
            )
            .expect("enqueue upload intent");
        let startup_reconcile = database
            .enqueue_startup_reconcile_intent(&watch_root, timestamp_ms(200))
            .expect("enqueue startup reconcile intent");

        assert_eq!(startup_reconcile.kind, PendingIntentKind::ReconcileSubtree);
        assert_eq!(startup_reconcile.available_at, UNIX_EPOCH);

        let leased = database
            .lease_next_ready(timestamp_ms(200))
            .expect("lease next ready")
            .expect("leased record");
        assert_eq!(leased.path, watch_root);
        assert_eq!(leased.kind, PendingIntentKind::ReconcileSubtree);
    }

    #[test]
    fn startup_reconcile_is_deduplicated_when_one_already_exists() {
        let temp_dir = TempDir::new().expect("temp dir");
        let database_path = temp_dir.path().join("state/vapor.sqlite");
        let mut database = DurableStateDb::open(&database_path).expect("open durable state db");
        let watch_root = PathBuf::from("/tmp/vapor-root");

        let first = database
            .enqueue_startup_reconcile_intent(&watch_root, timestamp_ms(100))
            .expect("enqueue first startup reconcile intent");
        let second = database
            .enqueue_startup_reconcile_intent(&watch_root, timestamp_ms(200))
            .expect("reuse startup reconcile intent");

        assert_eq!(database.queue_depth().expect("queue depth"), 1);
        assert_eq!(first.id, second.id);
        assert_eq!(second.available_at, UNIX_EPOCH);
    }

    #[test]
    fn startup_reconcile_reprioritizes_existing_delayed_root_reconcile() {
        let temp_dir = TempDir::new().expect("temp dir");
        let database_path = temp_dir.path().join("state/vapor.sqlite");
        let mut database = DurableStateDb::open(&database_path).expect("open durable state db");
        let watch_root = PathBuf::from("/tmp/vapor-root");

        let first = database
            .enqueue_startup_reconcile_intent(&watch_root, timestamp_ms(100))
            .expect("enqueue startup reconcile intent");
        assert!(
            database
                .lease_next_ready(timestamp_ms(100))
                .expect("lease next ready")
                .is_some()
        );
        assert!(
            database
                .requeue_leased(first.id, timestamp_ms(500), Some("yielded"))
                .expect("requeue yielded reconcile")
        );

        let reprioritized = database
            .enqueue_startup_reconcile_intent(&watch_root, timestamp_ms(600))
            .expect("reprioritize startup reconcile intent");

        assert_eq!(reprioritized.id, first.id);
        assert_eq!(reprioritized.available_at, UNIX_EPOCH);
        assert!(reprioritized.last_error.is_none());
    }

    #[test]
    fn leased_intent_is_recovered_after_reopen_for_at_least_once_replay() {
        let temp_dir = TempDir::new().expect("temp dir");
        let database_path = temp_dir.path().join("state/vapor.sqlite");
        let path = PathBuf::from("/tmp/vapor-root/project/file.txt");

        {
            let mut database = DurableStateDb::open(&database_path).expect("open durable state db");
            database
                .enqueue_intent(&path, PendingIntentKind::Delete, timestamp_ms(100))
                .expect("enqueue delete intent");
            let leased = database
                .lease_next_ready(timestamp_ms(100))
                .expect("lease next ready")
                .expect("leased record");
            assert_eq!(leased.attempt_count, 1);
        }

        let mut reopened = DurableStateDb::open(&database_path).expect("reopen durable state db");
        assert_eq!(reopened.leased_depth().expect("leased depth"), 1);
        assert_eq!(
            reopened
                .recover_leased(timestamp_ms(300))
                .expect("recover leased"),
            1
        );
        assert_eq!(reopened.pending_depth().expect("pending depth"), 1);

        let replayed = reopened
            .lease_next_ready(timestamp_ms(300))
            .expect("lease recovered record")
            .expect("replayed intent");
        assert_eq!(replayed.path, path);
        assert_eq!(replayed.kind, PendingIntentKind::Delete);
        assert_eq!(replayed.attempt_count, 2);
    }

    #[test]
    fn requeue_leased_intent_preserves_retry_metadata() {
        let temp_dir = TempDir::new().expect("temp dir");
        let database_path = temp_dir.path().join("state/vapor.sqlite");
        let mut database = DurableStateDb::open(&database_path).expect("open durable state db");
        let path = PathBuf::from("/tmp/vapor-root/project/file.txt");

        database
            .enqueue_intent(&path, PendingIntentKind::Rename, timestamp_ms(100))
            .expect("enqueue rename intent");
        let leased = database
            .lease_next_ready(timestamp_ms(100))
            .expect("lease next ready")
            .expect("leased record");

        assert!(
            database
                .requeue_leased(leased.id, timestamp_ms(500), Some("rate limited"))
                .expect("requeue leased")
        );
        assert!(
            database
                .lease_next_ready(timestamp_ms(300))
                .expect("lease before delay")
                .is_none()
        );

        let retried = database
            .lease_next_ready(timestamp_ms(500))
            .expect("lease retried intent")
            .expect("retried record");
        assert_eq!(retried.last_error.as_deref(), Some("rate limited"));
        assert_eq!(retried.attempt_count, 2);
    }

    #[test]
    fn schedule_retry_applies_transient_backoff_and_persists_error_text() {
        let temp_dir = TempDir::new().expect("temp dir");
        let database_path = temp_dir.path().join("state/vapor.sqlite");
        let mut database = DurableStateDb::open(&database_path).expect("open durable state db");
        let path = PathBuf::from("/tmp/vapor-root/project/file.txt");

        let queued = database
            .enqueue_intent(&path, PendingIntentKind::Upload, timestamp_ms(100))
            .expect("enqueue upload intent");
        let leased = database
            .lease_next_ready(timestamp_ms(100))
            .expect("lease next ready")
            .expect("leased record");
        assert_eq!(leased.id, queued.id);

        let scheduled = database
            .schedule_retry(
                leased.id,
                RetryFailureKind::Transient,
                "temporary network failure",
                timestamp_ms(100),
            )
            .expect("schedule transient retry")
            .expect("retry should be scheduled");

        assert_eq!(database.pending_depth().expect("pending depth"), 1);
        assert_eq!(database.leased_depth().expect("leased depth"), 0);
        assert_eq!(
            scheduled.intent.last_error.as_deref(),
            Some("temporary network failure")
        );
        assert!(scheduled.decision.delay.unwrap() >= Duration::from_millis(1_600));
        assert!(
            database
                .lease_next_ready(timestamp_ms(1_000))
                .expect("lease too early")
                .is_none()
        );

        let retried = database
            .lease_next_ready(scheduled.decision.available_at.unwrap())
            .expect("lease retried intent")
            .expect("retried record");
        assert_eq!(retried.attempt_count, 2);
    }

    #[test]
    fn schedule_retry_uses_slower_rate_limit_backoff() {
        let temp_dir = TempDir::new().expect("temp dir");
        let database_path = temp_dir.path().join("state/vapor.sqlite");
        let mut database = DurableStateDb::open(&database_path).expect("open durable state db");
        let path = PathBuf::from("/tmp/vapor-root/project/file.txt");

        database
            .enqueue_intent(&path, PendingIntentKind::Upload, timestamp_ms(100))
            .expect("enqueue upload intent");
        let leased = database
            .lease_next_ready(timestamp_ms(100))
            .expect("lease next ready")
            .expect("leased record");

        let scheduled = database
            .schedule_retry(
                leased.id,
                RetryFailureKind::RateLimited {
                    retry_after: Some(Duration::from_secs(30)),
                },
                "429 rate limited",
                timestamp_ms(100),
            )
            .expect("schedule rate-limited retry")
            .expect("retry should be scheduled");

        assert!(scheduled.decision.delay.unwrap() >= Duration::from_secs(30));
        assert_eq!(
            scheduled.decision.slowdown_until,
            scheduled.decision.available_at
        );
        assert!(
            database
                .lease_next_ready(timestamp_ms(10_000))
                .expect("lease before retry window")
                .is_none()
        );
    }

    #[test]
    fn rate_limit_slowdown_marker_survives_reopen_until_it_expires() {
        let temp_dir = TempDir::new().expect("temp dir");
        let database_path = temp_dir.path().join("state/vapor.sqlite");
        let path = PathBuf::from("/tmp/vapor-root/project/file.txt");

        {
            let mut database = DurableStateDb::open(&database_path).expect("open durable state db");
            database
                .enqueue_intent(&path, PendingIntentKind::Upload, timestamp_ms(100))
                .expect("enqueue upload intent");
            let leased = database
                .lease_next_ready(timestamp_ms(100))
                .expect("lease next ready")
                .expect("leased record");
            database
                .schedule_retry(
                    leased.id,
                    RetryFailureKind::RateLimited {
                        retry_after: Some(Duration::from_secs(30)),
                    },
                    "429 rate limited",
                    timestamp_ms(100),
                )
                .expect("schedule rate-limited retry")
                .expect("retry should be scheduled");
        }

        let mut reopened = DurableStateDb::open(&database_path).expect("reopen durable state db");
        assert!(
            reopened
                .take_active_retry_slowdown_until(timestamp_ms(10_000))
                .expect("load active slowdown marker")
                .is_some()
        );
        assert!(
            reopened
                .take_active_retry_slowdown_until(timestamp_ms(40_000))
                .expect("load expired slowdown marker")
                .is_none()
        );
        assert!(
            reopened
                .retry_slowdown_until()
                .expect("reload slowdown marker")
                .is_none()
        );
    }

    #[test]
    fn durable_retry_slowdown_keeps_the_longest_rate_limit_window() {
        let temp_dir = TempDir::new().expect("temp dir");
        let database_path = temp_dir.path().join("state/vapor.sqlite");
        let mut database = DurableStateDb::open(&database_path).expect("open durable state db");

        for (index, retry_after_seconds) in [3_600u64, 30u64].into_iter().enumerate() {
            let path = PathBuf::from(format!("/tmp/vapor-root/project/file-{index}.txt"));
            let now = timestamp_ms((index as u64 + 1) * 100);
            database
                .enqueue_intent(&path, PendingIntentKind::Upload, now)
                .expect("enqueue upload intent");
            let leased = database
                .lease_next_ready(now)
                .expect("lease next ready")
                .expect("leased record");
            database
                .schedule_retry(
                    leased.id,
                    RetryFailureKind::RateLimited {
                        retry_after: Some(Duration::from_secs(retry_after_seconds)),
                    },
                    "429 rate limited",
                    now,
                )
                .expect("schedule rate-limited retry")
                .expect("retry should be scheduled");
        }

        let persisted = database
            .retry_slowdown_until()
            .expect("load retry slowdown marker")
            .expect("persisted slowdown marker");
        assert_eq!(persisted, timestamp_ms(100) + Duration::from_secs(3_600));
    }

    #[test]
    fn finalize_leased_failure_moves_terminal_errors_out_of_active_queue() {
        let temp_dir = TempDir::new().expect("temp dir");
        let database_path = temp_dir.path().join("state/vapor.sqlite");
        let mut database = DurableStateDb::open(&database_path).expect("open durable state db");
        let path = PathBuf::from("/tmp/vapor-root/project/file.txt");

        database
            .enqueue_intent(&path, PendingIntentKind::Delete, timestamp_ms(100))
            .expect("enqueue delete intent");
        let leased = database
            .lease_next_ready(timestamp_ms(100))
            .expect("lease next ready")
            .expect("leased record");

        let failed = database
            .finalize_leased_failure(
                leased.id,
                RetryFailureKind::Permanent,
                "remote path missing permanently",
                timestamp_ms(200),
            )
            .expect("finalize terminal failure");

        assert_eq!(failed.path, path);
        assert_eq!(failed.failure_kind, RetryFailureKind::Permanent);
        assert_eq!(failed.last_error, "remote path missing permanently");
        assert_eq!(database.queue_depth().expect("queue depth"), 0);
        assert_eq!(database.failed_depth().expect("failed depth"), 1);
        assert!(
            database
                .intent_record(leased.id)
                .expect("queue record")
                .is_none()
        );
        assert!(
            database
                .failed_record(leased.id)
                .expect("failed record")
                .is_some()
        );
    }

    #[test]
    fn scheduled_retry_survives_reopen_and_still_respects_available_at() {
        let temp_dir = TempDir::new().expect("temp dir");
        let database_path = temp_dir.path().join("state/vapor.sqlite");
        let path = PathBuf::from("/tmp/vapor-root/project/file.txt");

        let scheduled_available_at = {
            let mut database = DurableStateDb::open(&database_path).expect("open durable state db");
            database
                .enqueue_intent(&path, PendingIntentKind::Delete, timestamp_ms(100))
                .expect("enqueue delete intent");
            let leased = database
                .lease_next_ready(timestamp_ms(100))
                .expect("lease next ready")
                .expect("leased record");

            database
                .schedule_retry(
                    leased.id,
                    RetryFailureKind::Transient,
                    "temporary backend failure",
                    timestamp_ms(100),
                )
                .expect("schedule transient retry")
                .expect("retry should be scheduled")
                .decision
                .available_at
                .expect("scheduled retry timestamp")
        };

        let mut reopened = DurableStateDb::open(&database_path).expect("reopen durable state db");
        assert!(
            reopened
                .lease_next_ready(timestamp_ms(1_000))
                .expect("lease too early after reopen")
                .is_none()
        );
        let retried = reopened
            .lease_next_ready(scheduled_available_at)
            .expect("lease scheduled retry after reopen")
            .expect("retried record");
        assert_eq!(retried.path, path);
        assert_eq!(
            retried.last_error.as_deref(),
            Some("temporary backend failure")
        );
        assert_eq!(retried.attempt_count, 2);
    }

    #[test]
    fn schedule_retry_rejects_non_retryable_failures() {
        let temp_dir = TempDir::new().expect("temp dir");
        let database_path = temp_dir.path().join("state/vapor.sqlite");
        let mut database = DurableStateDb::open(&database_path).expect("open durable state db");
        let path = PathBuf::from("/tmp/vapor-root/project/file.txt");

        database
            .enqueue_intent(&path, PendingIntentKind::Delete, timestamp_ms(100))
            .expect("enqueue delete intent");
        let leased = database
            .lease_next_ready(timestamp_ms(100))
            .expect("lease next ready")
            .expect("leased record");

        let error = database
            .schedule_retry(
                leased.id,
                RetryFailureKind::Permanent,
                "permanent failure",
                timestamp_ms(100),
            )
            .expect_err("permanent failures should be finalized outside retry scheduling");

        assert!(matches!(error, StateDbError::InvalidIntentState(_)));
        assert_eq!(database.leased_depth().expect("leased depth"), 1);
        assert_eq!(database.pending_depth().expect("pending depth"), 0);
    }

    #[test]
    fn state_entries_survive_reopen() {
        let temp_dir = TempDir::new().expect("temp dir");
        let database_path = temp_dir.path().join("state/vapor.sqlite");

        {
            let mut database = DurableStateDb::open(&database_path).expect("open durable state db");
            let entry = database
                .set_state("queue.resume_marker", "cursor-123", timestamp_ms(100))
                .expect("set state");
            assert_eq!(entry.value, "cursor-123");
        }

        let mut reopened = DurableStateDb::open(&database_path).expect("reopen durable state db");
        let entry = reopened
            .state("queue.resume_marker")
            .expect("load state entry")
            .expect("existing state entry");
        assert_eq!(entry.value, "cursor-123");
        assert!(
            reopened
                .delete_state("queue.resume_marker")
                .expect("delete state entry")
        );
        assert!(
            reopened
                .state("queue.resume_marker")
                .expect("load state")
                .is_none()
        );
    }

    #[test]
    fn non_utf8_paths_round_trip_without_loss() {
        let temp_dir = TempDir::new().expect("temp dir");
        let database_path = temp_dir.path().join("state/vapor.sqlite");
        let mut database = DurableStateDb::open(&database_path).expect("open durable state db");
        let path = PathBuf::from(std::ffi::OsString::from_vec(vec![0x66, 0x6f, 0x80]));

        let queued = database
            .enqueue_intent(&path, PendingIntentKind::Upload, timestamp_ms(100))
            .expect("enqueue intent");

        assert_eq!(path_to_bytes(&queued.path), vec![0x66, 0x6f, 0x80]);
    }

    #[test]
    fn unsupported_schema_version_is_rejected() {
        let temp_dir = TempDir::new().expect("temp dir");
        let database_path = temp_dir.path().join("state/vapor.sqlite");
        if let Some(parent) = database_path.parent() {
            fs::create_dir_all(parent).expect("create parent directory");
        }

        let connection = Connection::open(&database_path).expect("open sqlite connection");
        configure_connection(&connection).expect("configure connection");
        connection
            .execute_batch(
                "CREATE TABLE schema_meta (
                     singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
                     schema_version INTEGER NOT NULL CHECK(schema_version > 0)
                 );
                 INSERT INTO schema_meta (singleton, schema_version) VALUES (1, 99);",
            )
            .expect("seed future schema version");
        drop(connection);

        let error = DurableStateDb::open(&database_path).expect_err("schema mismatch should fail");
        assert!(matches!(
            error,
            StateDbError::SchemaVersionMismatch {
                found: 99,
                expected: CURRENT_SCHEMA_VERSION
            }
        ));
    }

    #[test]
    fn version_one_database_migrates_forward_to_version_two() {
        let temp_dir = TempDir::new().expect("temp dir");
        let database_path = temp_dir.path().join("state/vapor.sqlite");
        if let Some(parent) = database_path.parent() {
            fs::create_dir_all(parent).expect("create parent directory");
        }

        let connection = Connection::open(&database_path).expect("open sqlite connection");
        configure_connection(&connection).expect("configure connection");
        connection
            .execute_batch(
                "CREATE TABLE schema_meta (
                     singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
                     schema_version INTEGER NOT NULL CHECK(schema_version > 0)
                 );
                 INSERT INTO schema_meta (singleton, schema_version) VALUES (1, 1);
                 CREATE TABLE queue_intents (
                     id INTEGER PRIMARY KEY AUTOINCREMENT,
                     path_bytes BLOB NOT NULL,
                     kind TEXT NOT NULL CHECK(kind IN ('upload', 'delete', 'rename', 'reconcile_subtree')),
                     state TEXT NOT NULL CHECK(state IN ('pending', 'leased')),
                     enqueued_at_ms INTEGER NOT NULL,
                     available_at_ms INTEGER NOT NULL,
                     leased_at_ms INTEGER,
                     attempt_count INTEGER NOT NULL DEFAULT 0 CHECK(attempt_count >= 0),
                     last_error TEXT
                 );
                 CREATE INDEX idx_queue_intents_ready ON queue_intents(state, available_at_ms, id);
                 CREATE TABLE state_entries (
                     key TEXT PRIMARY KEY,
                     value TEXT NOT NULL,
                     updated_at_ms INTEGER NOT NULL
                 );",
            )
            .expect("seed version one schema");
        drop(connection);

        let database = DurableStateDb::open(&database_path).expect("migrate version one database");
        assert_eq!(
            database.schema_version().expect("schema version"),
            CURRENT_SCHEMA_VERSION
        );
        assert_eq!(database.failed_depth().expect("failed depth"), 0);
    }

    fn timestamp_ms(milliseconds: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_millis(milliseconds)
    }
}
