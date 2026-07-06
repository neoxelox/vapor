use std::error::Error;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use vapor_shared::{constants, logging::sanitize_diagnostic_text, runtime_paths};

use crate::event_intents::PendingIntentKind;
use crate::retry::{RetryDecision, RetryFailureKind, RetryPolicy};

const CURRENT_SCHEMA_VERSION: i64 = 4;
/// The last schema version this build can migrate forward in place.
/// v3 → v4 widened the intent-kind vocabulary with the remote→local
/// pipeline kinds (`download`, `apply_remote_delete`; C8-6), which only
/// requires recreating the two intent tables with the wider CHECK.
const MIGRATABLE_SCHEMA_VERSION: i64 = 3;
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
    InvalidAttemptCount(i64),
    InvalidStateValue(String),
    InvalidIntentKind(String),
    InvalidIntentState(String),
    TimeBeforeUnixEpoch,
    MissingIntentRecord(i64),
    NonUtf8Path(String),
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
            Self::InvalidAttemptCount(value) => write!(f, "invalid attempt count value {value}"),
            Self::InvalidStateValue(message) => write!(f, "invalid state value: {message}"),
            Self::InvalidIntentKind(kind) => write!(f, "invalid intent kind '{kind}'"),
            Self::InvalidIntentState(state) => write!(f, "invalid intent state '{state}'"),
            Self::TimeBeforeUnixEpoch => write!(f, "time before UNIX epoch is unsupported"),
            Self::MissingIntentRecord(id) => write!(f, "missing durable intent record {id}"),
            Self::NonUtf8Path(display) => {
                write!(f, "non-UTF-8 path is not storable: {display}")
            }
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

    /// Opens the durable DB with the documented corruption-recovery
    /// path (AGENTS.md §5): when the file is not a readable SQLite
    /// database, it is quarantined next to itself
    /// (`vapor.sqlite.corrupt-<ms>`) and a fresh database takes its
    /// place. The startup whole-scope reconcile reconstructs intent
    /// state conservatively; the quarantined file stays on disk for
    /// support inspection. Version mismatches are NOT recovered this
    /// way — an incompatible schema is a real error, not corruption.
    pub fn open_with_corruption_recovery(
        path: impl AsRef<Path>,
        now: SystemTime,
    ) -> Result<Self, StateDbError> {
        let path = path.as_ref().to_path_buf();
        match Self::open(&path) {
            Ok(database) => Ok(database),
            Err(StateDbError::Sql(error)) => {
                let now_ms = system_time_to_millis(now).unwrap_or(0);
                let quarantine = path.with_extension(format!("sqlite.corrupt-{now_ms}"));
                crate::logging::error(
                    "Durable state DB is corrupt; quarantining it and starting fresh",
                    &[
                        ("database_path", path.display().to_string()),
                        ("quarantine_path", quarantine.display().to_string()),
                        ("error", error.to_string()),
                    ],
                );
                std::fs::rename(&path, &quarantine)?;
                // WAL/SHM siblings belong to the corrupt database.
                for suffix in ["-wal", "-shm"] {
                    let mut sibling = path.clone().into_os_string();
                    sibling.push(suffix);
                    let _ = std::fs::remove_file(std::path::PathBuf::from(sibling));
                }
                Self::open(&path)
            }
            Err(error) => Err(error),
        }
    }

    pub fn open(path: impl AsRef<Path>) -> Result<Self, StateDbError> {
        let path = path.as_ref().to_path_buf();
        runtime_paths::ensure_private_file(&path)?;

        let mut connection = Connection::open(&path)?;
        runtime_paths::ensure_private_file(&path)?;
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

    /// The oldest `limit` queue rows (pending and leased) for the
    /// per-intent diagnostics surface (C8-29).
    pub fn list_queue_intents(
        &self,
        limit: usize,
    ) -> Result<Vec<DurableIntentRecord>, StateDbError> {
        let mut statement = self.connection.prepare(
            "SELECT id, path_text, kind, enqueued_at_ms, available_at_ms,
                    leased_at_ms, attempt_count, last_error
             FROM queue_intents
             ORDER BY available_at_ms, id
             LIMIT ?",
        )?;
        let rows =
            statement.query_map(params![i64::try_from(limit).unwrap_or(i64::MAX)], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, Option<i64>>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, Option<String>>(7)?,
                ))
            })?;
        let mut records = Vec::new();
        for row in rows {
            let (
                id,
                path_text,
                kind,
                enqueued_at_ms,
                available_at_ms,
                leased_at_ms,
                attempt_count,
                last_error,
            ) = row?;
            records.push(DurableIntentRecord {
                id,
                path: PathBuf::from(path_text),
                kind: intent_kind_from_label(&kind)?,
                enqueued_at: millis_to_system_time(enqueued_at_ms)?,
                available_at: millis_to_system_time(available_at_ms)?,
                leased_at: leased_at_ms.map(millis_to_system_time).transpose()?,
                attempt_count: validate_attempt_count(attempt_count)?,
                last_error,
            });
        }
        Ok(records)
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
        let path_text = path_to_text(path)?;
        let existing = transaction
            .query_row(
                "SELECT id, state
                 FROM queue_intents
                 WHERE path_text = ? AND kind = ? AND state IN (?, ?)
                 ORDER BY available_at_ms ASC, id ASC
                 LIMIT 1",
                params![
                    path_text,
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
        Ok(self.lease_ready_batch(now, 1)?.into_iter().next())
    }

    pub fn peek_next_ready_kind(
        &self,
        now: SystemTime,
    ) -> Result<Option<PendingIntentKind>, StateDbError> {
        let now_ms = system_time_to_millis(now)?;
        let raw_kind = self
            .connection
            .query_row(
                "SELECT kind
             FROM queue_intents
             WHERE state = ? AND available_at_ms <= ?
             ORDER BY available_at_ms ASC, id ASC
             LIMIT 1",
                params![STATE_PENDING, now_ms],
                |row| row.get::<_, String>(0),
            )
            .optional()?;

        raw_kind
            .map(|kind| intent_kind_from_label(&kind))
            .transpose()
    }

    pub fn lease_ready_batch(
        &mut self,
        now: SystemTime,
        limit: usize,
    ) -> Result<Vec<DurableIntentRecord>, StateDbError> {
        if limit == 0 {
            return Ok(Vec::new());
        }

        let now_ms = system_time_to_millis(now)?;
        // One atomic UPDATE … RETURNING instead of a select + per-row
        // update + per-row re-fetch: a single statement, a single
        // implicit transaction. RETURNING row order is unspecified, so
        // ready order is restored in memory below.
        let mut statement = self.connection.prepare(
            "UPDATE queue_intents
             SET state = ?, leased_at_ms = ?
             WHERE id IN (
                 SELECT id FROM queue_intents
                 WHERE state = ? AND available_at_ms <= ?
                 ORDER BY available_at_ms ASC, id ASC
                 LIMIT ?
             )
             RETURNING id, path_text, kind, enqueued_at_ms, available_at_ms, leased_at_ms,
                       attempt_count, last_error",
        )?;
        let mut leased_intents: Vec<DurableIntentRecord> = statement
            .query_map(
                params![STATE_LEASED, now_ms, STATE_PENDING, now_ms, limit as i64],
                intent_from_row,
            )?
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .map(raw_intent_to_record)
            .collect::<Result<_, _>>()?;
        drop(statement);

        leased_intents.sort_by_key(|intent| (intent.available_at, intent.id));
        Ok(leased_intents)
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
            params![
                STATE_PENDING,
                available_at_ms,
                last_error.map(sanitize_persisted_error),
                id,
                STATE_LEASED
            ],
        )?;
        Ok(changed > 0)
    }

    fn requeue_leased_with_attempt_bump(
        &mut self,
        id: i64,
        available_at: SystemTime,
        last_error: Option<&str>,
    ) -> Result<bool, StateDbError> {
        let available_at_ms = system_time_to_millis(available_at)?;
        let changed = self.connection.execute(
            "UPDATE queue_intents
             SET state = ?,
                 available_at_ms = ?,
                 leased_at_ms = NULL,
                 last_error = ?,
                 attempt_count = attempt_count + 1
             WHERE id = ? AND state = ?",
            params![
                STATE_PENDING,
                available_at_ms,
                last_error.map(sanitize_persisted_error),
                id,
                STATE_LEASED
            ],
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
        if leased_intent.attempt_count >= constants::state::MAX_ATTEMPT_COUNT {
            return Err(StateDbError::InvalidIntentState(format!(
                "intent {id} reached MAX_ATTEMPT_COUNT {}; caller must finalize as terminal failure",
                constants::state::MAX_ATTEMPT_COUNT
            )));
        }
        let next_attempt_count = leased_intent.attempt_count.saturating_add(1);
        let decision =
            RetryPolicy::default().decide(leased_intent.id, next_attempt_count, failure_kind, now);
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
        if !self.requeue_leased_with_attempt_bump(id, available_at, Some(last_error))? {
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
                 path_text,
                 kind,
                 failure_kind,
                 enqueued_at_ms,
                 failed_at_ms,
                 attempt_count,
                 last_error
             )
             SELECT
                 id,
                 path_text,
                 kind,
                 ?,
                 enqueued_at_ms,
                 ?,
                 attempt_count,
                 ?
             FROM queue_intents
             WHERE id = ? AND state = ?",
            params![
                failure_label,
                failed_at_ms,
                sanitize_persisted_error(last_error),
                id,
                STATE_LEASED
            ],
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

    /// Startup recovery: every lease belongs to a dead process, so all of
    /// them are returned to `pending`. Stale leases (older than
    /// [`constants::engine::LEASE_TIMEOUT_MILLIS`]) additionally reset
    /// `attempt_count` — documented in `data-flow.md` §Local to remote —
    /// while fresh leases keep their retry history.
    pub fn recover_leased(&mut self, now: SystemTime) -> Result<usize, StateDbError> {
        let stale_leases_count = self.recover_stale_leases(now)?;
        let now_ms = system_time_to_millis(now)?;
        let fresh_leases_count = self.connection.execute(
            "UPDATE queue_intents
             SET state = ?, available_at_ms = ?, leased_at_ms = NULL
             WHERE state = ?",
            params![STATE_PENDING, now_ms, STATE_LEASED],
        )?;
        Ok(stale_leases_count + fresh_leases_count)
    }

    /// In-run recovery sweep: returns only leases older than the lease
    /// timeout to `pending`. Safe to call while the daemon is live —
    /// legitimately in-flight leases are far younger than the 15-minute
    /// timeout, so only orphaned leases (a lost execution) are replayed.
    pub fn recover_stale_leases(&mut self, now: SystemTime) -> Result<usize, StateDbError> {
        let now_ms = system_time_to_millis(now)?;
        let stale_lease_cutoff_ms = now_ms
            .saturating_sub(i64::try_from(constants::engine::LEASE_TIMEOUT_MILLIS).unwrap_or(0));
        let stale_leases_count = self.connection.execute(
            "UPDATE queue_intents
             SET state = ?,
                 available_at_ms = ?,
                 leased_at_ms = NULL,
                 last_error = ?,
                 attempt_count = 0
             WHERE state = ?
               AND leased_at_ms IS NOT NULL
               AND leased_at_ms <= ?",
            params![
                STATE_PENDING,
                now_ms,
                sanitize_persisted_error("lease recovered after exceeding LEASE_TIMEOUT_MILLIS"),
                STATE_LEASED,
                stale_lease_cutoff_ms
            ],
        )?;
        Ok(stale_leases_count)
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
        validate_state_key(key)?;
        validate_state_value(value)?;
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
                validate_state_key(&entry_key)?;
                validate_state_value(&value)?;
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

    /// Enqueues a batch of intents in one transaction, coalescing each
    /// against an existing **pending** row with the same `(path, kind)`:
    /// a path that is already queued (including one waiting out a retry
    /// backoff) does not grow a duplicate row. Leased rows do not
    /// coalesce — work already in flight may have read stale content, so
    /// a fresh pending row is the correct "run again after" signal.
    ///
    /// Returns the number of rows actually inserted.
    pub fn enqueue_intents_coalesced(
        &mut self,
        intents: &[(PathBuf, PendingIntentKind, SystemTime)],
    ) -> Result<usize, StateDbError> {
        if intents.is_empty() {
            return Ok(0);
        }

        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut inserted = 0usize;
        for (path, kind, observed_at) in intents {
            let path_text = path_to_text(path)?;
            let existing_pending = transaction
                .query_row(
                    "SELECT id FROM queue_intents
                     WHERE path_text = ? AND kind = ? AND state = ?
                     LIMIT 1",
                    params![path_text, intent_kind_label(*kind), STATE_PENDING],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?;
            if existing_pending.is_some() {
                continue;
            }
            insert_intent(&transaction, path, *kind, *observed_at, *observed_at)?;
            inserted += 1;
        }
        transaction.commit()?;
        Ok(inserted)
    }
}

/// Per-path last-synced state (C8-14/C8-17). One row per path that has
/// completed a transfer in either direction; the conflict machinery
/// compares current local/remote state against it to distinguish
/// "unchanged since last sync" from "concurrently modified".
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SyncIndexEntry {
    pub path: PathBuf,
    pub content_hash: String,
    pub size_bytes: u64,
    /// Local mtime at the moment the transfer completed; the cheap
    /// pre-filter for local-divergence checks (rsync-style quick check).
    pub local_modified_at: Option<SystemTime>,
    pub last_op_id: String,
    pub updated_at: SystemTime,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TombstoneOrigin {
    /// The deletion originated locally (propagates to the provider).
    Local,
    /// The deletion originated remotely (applied to the local replica).
    Remote,
}

impl TombstoneOrigin {
    fn label(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Remote => "remote",
        }
    }

    fn from_label(label: &str) -> Result<Self, StateDbError> {
        match label {
            "local" => Ok(Self::Local),
            "remote" => Ok(Self::Remote),
            other => Err(StateDbError::InvalidStateValue(format!(
                "invalid tombstone origin '{other}'"
            ))),
        }
    }
}

/// Durable deletion marker with restart-safe replay semantics (C8-16).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TombstoneRecord {
    pub path: PathBuf,
    pub origin: TombstoneOrigin,
    pub deleted_at: SystemTime,
}

impl DurableStateDb {
    pub fn set_sync_index(
        &mut self,
        path: &Path,
        content_hash: &str,
        size_bytes: u64,
        local_modified_at: Option<SystemTime>,
        last_op_id: &str,
        now: SystemTime,
    ) -> Result<(), StateDbError> {
        let local_modified_at_ms = local_modified_at.map(system_time_to_millis).transpose()?;
        self.connection.execute(
            "INSERT INTO sync_index
                 (path_text, content_hash, size_bytes, local_modified_at_ms, last_op_id, updated_at_ms)
             VALUES (?, ?, ?, ?, ?, ?)
             ON CONFLICT(path_text) DO UPDATE SET
                 content_hash = excluded.content_hash,
                 size_bytes = excluded.size_bytes,
                 local_modified_at_ms = excluded.local_modified_at_ms,
                 last_op_id = excluded.last_op_id,
                 updated_at_ms = excluded.updated_at_ms",
            params![
                path_to_text(path)?,
                content_hash,
                i64::try_from(size_bytes).unwrap_or(i64::MAX),
                local_modified_at_ms,
                last_op_id,
                system_time_to_millis(now)?
            ],
        )?;
        Ok(())
    }

    pub fn sync_index(&self, path: &Path) -> Result<Option<SyncIndexEntry>, StateDbError> {
        let path_text = path_to_text(path)?;
        let row = self
            .connection
            .query_row(
                "SELECT content_hash, size_bytes, local_modified_at_ms, last_op_id, updated_at_ms
                 FROM sync_index WHERE path_text = ?",
                params![path_text],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, Option<i64>>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                    ))
                },
            )
            .optional()?;
        row.map(
            |(content_hash, size_bytes, local_modified_at_ms, last_op_id, updated_at_ms)| {
                Ok(SyncIndexEntry {
                    path: path.to_path_buf(),
                    content_hash,
                    size_bytes: u64::try_from(size_bytes).unwrap_or(0),
                    local_modified_at: local_modified_at_ms
                        .map(millis_to_system_time)
                        .transpose()?,
                    last_op_id,
                    updated_at: millis_to_system_time(updated_at_ms)?,
                })
            },
        )
        .transpose()
    }

    pub fn remove_sync_index(&mut self, path: &Path) -> Result<(), StateDbError> {
        self.connection.execute(
            "DELETE FROM sync_index WHERE path_text = ?",
            params![path_to_text(path)?],
        )?;
        Ok(())
    }

    /// Records a deletion marker; a later deletion for the same path
    /// replaces the older one.
    pub fn record_tombstone(
        &mut self,
        path: &Path,
        origin: TombstoneOrigin,
        now: SystemTime,
    ) -> Result<(), StateDbError> {
        self.connection.execute(
            "INSERT INTO tombstones (path_text, origin, deleted_at_ms)
             VALUES (?, ?, ?)
             ON CONFLICT(path_text) DO UPDATE SET
                 origin = excluded.origin,
                 deleted_at_ms = excluded.deleted_at_ms",
            params![
                path_to_text(path)?,
                origin.label(),
                system_time_to_millis(now)?
            ],
        )?;
        Ok(())
    }

    pub fn tombstone(&self, path: &Path) -> Result<Option<TombstoneRecord>, StateDbError> {
        let path_text = path_to_text(path)?;
        let row = self
            .connection
            .query_row(
                "SELECT origin, deleted_at_ms FROM tombstones WHERE path_text = ?",
                params![path_text],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional()?;
        row.map(|(origin, deleted_at_ms)| {
            Ok(TombstoneRecord {
                path: path.to_path_buf(),
                origin: TombstoneOrigin::from_label(&origin)?,
                deleted_at: millis_to_system_time(deleted_at_ms)?,
            })
        })
        .transpose()
    }

    pub fn clear_tombstone(&mut self, path: &Path) -> Result<(), StateDbError> {
        self.connection.execute(
            "DELETE FROM tombstones WHERE path_text = ?",
            params![path_to_text(path)?],
        )?;
        Ok(())
    }

    /// Startup hygiene: drops tombstones past the retention window so
    /// the table stays bounded (C8-16).
    pub fn prune_tombstones(&mut self, now: SystemTime) -> Result<usize, StateDbError> {
        let now_ms = system_time_to_millis(now)?;
        let cutoff = now_ms.saturating_sub(
            i64::try_from(constants::state::TOMBSTONE_RETENTION_MILLIS).unwrap_or(i64::MAX),
        );
        let pruned = self.connection.execute(
            "DELETE FROM tombstones WHERE deleted_at_ms < ?",
            params![cutoff],
        )?;
        Ok(pruned)
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
            path_text,
            kind,
            state,
            enqueued_at_ms,
            available_at_ms,
            leased_at_ms,
            attempt_count,
            last_error
        ) VALUES (?, ?, ?, ?, ?, NULL, 0, NULL)",
        params![
            path_to_text(path)?,
            intent_kind_label(kind),
            STATE_PENDING,
            enqueued_at_ms,
            available_at_ms
        ],
    )?;
    Ok(connection.last_insert_rowid())
}

fn configure_connection(connection: &Connection) -> Result<(), StateDbError> {
    // `synchronous = NORMAL` is the documented durability point for WAL
    // mode: the database can never corrupt, and at most the final
    // transaction(s) before an OS crash / power loss are rolled back.
    // The queue's at-least-once semantics plus the startup whole-scope
    // reconcile already reconstruct anything lost that way, so paying
    // `FULL`'s per-commit fsync bought nothing the design needs.
    connection.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         PRAGMA foreign_keys = ON;
         PRAGMA busy_timeout = 5000;",
    )?;
    Ok(())
}

fn migrate_schema(connection: &mut Connection) -> Result<(), StateDbError> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;

    // Validate the recorded schema version BEFORE running any DDL: an
    // old-layout database must fail with the clean `SchemaVersionMismatch`
    // instead of whatever confusing SQLite error a conflicting
    // `CREATE TABLE / INDEX` would produce first.
    let schema_meta_exists = transaction
        .query_row(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'schema_meta'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .is_some();
    if schema_meta_exists {
        match read_schema_version(&transaction)? {
            Some(CURRENT_SCHEMA_VERSION) | None => {}
            Some(MIGRATABLE_SCHEMA_VERSION) => {
                migrate_v3_to_v4(&transaction)?;
            }
            Some(found) => {
                return Err(StateDbError::SchemaVersionMismatch {
                    found,
                    expected: CURRENT_SCHEMA_VERSION,
                });
            }
        }
    }

    transaction.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_meta (
             singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
             schema_version INTEGER NOT NULL CHECK(schema_version > 0)
         );
         CREATE TABLE IF NOT EXISTS queue_intents (
             id INTEGER PRIMARY KEY AUTOINCREMENT,
             path_text TEXT NOT NULL,
             kind TEXT NOT NULL CHECK(kind IN ('upload', 'delete', 'rename', 'download', 'apply_remote_delete', 'reconcile_subtree')),
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
             path_text TEXT NOT NULL,
             kind TEXT NOT NULL CHECK(kind IN ('upload', 'delete', 'rename', 'download', 'apply_remote_delete', 'reconcile_subtree')),
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
         );
         CREATE TABLE IF NOT EXISTS sync_index (
             path_text TEXT PRIMARY KEY,
             content_hash TEXT NOT NULL,
             size_bytes INTEGER NOT NULL CHECK(size_bytes >= 0),
             local_modified_at_ms INTEGER,
             last_op_id TEXT NOT NULL,
             updated_at_ms INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS tombstones (
             path_text TEXT PRIMARY KEY,
             origin TEXT NOT NULL CHECK(origin IN ('local', 'remote')),
             deleted_at_ms INTEGER NOT NULL
         );",
    )?;

    if read_schema_version(&transaction)?.is_none() {
        transaction.execute(
            "INSERT INTO schema_meta (singleton, schema_version) VALUES (1, ?)",
            params![CURRENT_SCHEMA_VERSION],
        )?;
    }

    transaction.commit()?;
    Ok(())
}

/// Forward migration v3 → v4: rebuilds the two intent tables with the
/// widened `kind` CHECK (SQLite cannot alter CHECK constraints in
/// place) while preserving every row and the AUTOINCREMENT sequence.
/// Rollback story: v4 rows using the new kinds cannot exist in a v3
/// database, so rolling back to a v3 build after remote-sourced intents
/// were enqueued is unsupported — pre-GA policy (AGENTS.md §1.1) with
/// the change documented in `docs/architecture/state-schema-migrations.md`.
fn migrate_v3_to_v4(transaction: &rusqlite::Transaction<'_>) -> Result<(), StateDbError> {
    transaction.execute_batch(
        "CREATE TABLE queue_intents_v4 (
             id INTEGER PRIMARY KEY AUTOINCREMENT,
             path_text TEXT NOT NULL,
             kind TEXT NOT NULL CHECK(kind IN ('upload', 'delete', 'rename', 'download', 'apply_remote_delete', 'reconcile_subtree')),
             state TEXT NOT NULL CHECK(state IN ('pending', 'leased')),
             enqueued_at_ms INTEGER NOT NULL,
             available_at_ms INTEGER NOT NULL,
             leased_at_ms INTEGER,
             attempt_count INTEGER NOT NULL DEFAULT 0 CHECK(attempt_count >= 0),
             last_error TEXT
         );
         INSERT INTO queue_intents_v4
             SELECT id, path_text, kind, state, enqueued_at_ms, available_at_ms,
                    leased_at_ms, attempt_count, last_error
             FROM queue_intents;
         DROP TABLE queue_intents;
         ALTER TABLE queue_intents_v4 RENAME TO queue_intents;
         CREATE INDEX IF NOT EXISTS idx_queue_intents_ready
             ON queue_intents(state, available_at_ms, id);
         CREATE TABLE failed_intents_v4 (
             id INTEGER PRIMARY KEY,
             path_text TEXT NOT NULL,
             kind TEXT NOT NULL CHECK(kind IN ('upload', 'delete', 'rename', 'download', 'apply_remote_delete', 'reconcile_subtree')),
             failure_kind TEXT NOT NULL CHECK(failure_kind IN ('authentication', 'permanent')),
             enqueued_at_ms INTEGER NOT NULL,
             failed_at_ms INTEGER NOT NULL,
             attempt_count INTEGER NOT NULL CHECK(attempt_count >= 0),
             last_error TEXT NOT NULL
         );
         INSERT INTO failed_intents_v4
             SELECT id, path_text, kind, failure_kind, enqueued_at_ms, failed_at_ms,
                    attempt_count, last_error
             FROM failed_intents;
         DROP TABLE failed_intents;
         ALTER TABLE failed_intents_v4 RENAME TO failed_intents;
         UPDATE schema_meta SET schema_version = 4 WHERE singleton = 1;",
    )?;
    crate::logging::info(
        "Migrated durable state schema v3 -> v4 (remote-sourced intent kinds)",
        &[],
    );
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

/// Raw column tuple for a `queue_intents` row, in the canonical
/// `id, path_text, kind, enqueued_at_ms, available_at_ms, leased_at_ms,
/// attempt_count, last_error` order.
type RawIntentRow = (
    i64,
    String,
    String,
    i64,
    i64,
    Option<i64>,
    i64,
    Option<String>,
);

fn intent_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawIntentRow> {
    Ok((
        row.get::<_, i64>(0)?,
        row.get::<_, String>(1)?,
        row.get::<_, String>(2)?,
        row.get::<_, i64>(3)?,
        row.get::<_, i64>(4)?,
        row.get::<_, Option<i64>>(5)?,
        row.get::<_, i64>(6)?,
        row.get::<_, Option<String>>(7)?,
    ))
}

fn raw_intent_to_record(raw: RawIntentRow) -> Result<DurableIntentRecord, StateDbError> {
    let (
        id,
        path_text,
        kind,
        enqueued_at_ms,
        available_at_ms,
        leased_at_ms,
        attempt_count,
        last_error,
    ) = raw;
    Ok(DurableIntentRecord {
        id,
        path: path_from_text(path_text),
        kind: intent_kind_from_label(&kind)?,
        enqueued_at: millis_to_system_time(enqueued_at_ms)?,
        available_at: millis_to_system_time(available_at_ms)?,
        leased_at: leased_at_ms.map(millis_to_system_time).transpose()?,
        attempt_count: validate_attempt_count(attempt_count)?,
        last_error: last_error.map(|value| sanitize_persisted_error(&value)),
    })
}

fn fetch_intent(
    connection: &Connection,
    id: i64,
) -> Result<Option<DurableIntentRecord>, StateDbError> {
    let raw_intent = connection
        .query_row(
            "SELECT id, path_text, kind, enqueued_at_ms, available_at_ms, leased_at_ms, attempt_count, last_error
             FROM queue_intents
             WHERE id = ?",
            params![id],
            intent_from_row,
        )
        .optional()?;

    raw_intent.map(raw_intent_to_record).transpose()
}

fn fetch_failed_intent(
    connection: &Connection,
    id: i64,
) -> Result<Option<DurableFailedIntentRecord>, StateDbError> {
    let raw_intent = connection
        .query_row(
            "SELECT id, path_text, kind, failure_kind, enqueued_at_ms, failed_at_ms, attempt_count, last_error
             FROM failed_intents
             WHERE id = ?",
            params![id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
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
                path_text,
                kind,
                failure_kind,
                enqueued_at_ms,
                failed_at_ms,
                attempt_count,
                last_error,
            )| {
                Ok(DurableFailedIntentRecord {
                    id: row_id,
                    path: path_from_text(path_text),
                    kind: intent_kind_from_label(&kind)?,
                    failure_kind: terminal_failure_from_label(&failure_kind)?,
                    enqueued_at: millis_to_system_time(enqueued_at_ms)?,
                    failed_at: millis_to_system_time(failed_at_ms)?,
                    attempt_count: validate_attempt_count(attempt_count)?,
                    last_error: sanitize_persisted_error(&last_error),
                })
            },
        )
        .transpose()
}

fn path_to_text(path: &Path) -> Result<String, StateDbError> {
    path.to_str()
        .map(|s| s.to_string())
        .ok_or_else(|| StateDbError::NonUtf8Path(path.display().to_string()))
}

fn path_from_text(text: String) -> PathBuf {
    PathBuf::from(text)
}

fn system_time_to_millis(time: SystemTime) -> Result<i64, StateDbError> {
    let duration = time
        .duration_since(UNIX_EPOCH)
        .map_err(|_| StateDbError::TimeBeforeUnixEpoch)?;
    let millis = i64::try_from(duration.as_millis())
        .map_err(|_| StateDbError::InvalidTimestampMillis(i64::MAX))?;
    if !(0..=constants::state::MAX_TIMESTAMP_MILLIS).contains(&millis) {
        return Err(StateDbError::InvalidTimestampMillis(millis));
    }
    Ok(millis)
}

fn millis_to_system_time(millis: i64) -> Result<SystemTime, StateDbError> {
    if !(0..=constants::state::MAX_TIMESTAMP_MILLIS).contains(&millis) {
        return Err(StateDbError::InvalidTimestampMillis(millis));
    }
    Ok(UNIX_EPOCH + Duration::from_millis(millis as u64))
}

fn sanitize_persisted_error(raw: &str) -> String {
    let sanitized = sanitize_diagnostic_text(raw);
    sanitized
        .chars()
        .take(constants::state::MAX_DIAGNOSTIC_TEXT_LENGTH)
        .collect()
}

fn validate_attempt_count(value: i64) -> Result<u32, StateDbError> {
    if !(0..=i64::from(constants::state::MAX_ATTEMPT_COUNT)).contains(&value) {
        return Err(StateDbError::InvalidAttemptCount(value));
    }

    Ok(value as u32)
}

fn validate_state_key(key: &str) -> Result<(), StateDbError> {
    if key.is_empty() || key.len() > constants::state::MAX_STATE_KEY_LENGTH {
        return Err(StateDbError::InvalidStateValue(format!(
            "state key length {} is outside 1..={}",
            key.len(),
            constants::state::MAX_STATE_KEY_LENGTH
        )));
    }

    Ok(())
}

fn validate_state_value(value: &str) -> Result<(), StateDbError> {
    if value.len() > constants::state::MAX_STATE_VALUE_LENGTH {
        return Err(StateDbError::InvalidStateValue(format!(
            "state value length {} exceeds {}",
            value.len(),
            constants::state::MAX_STATE_VALUE_LENGTH
        )));
    }

    Ok(())
}

fn intent_kind_label(kind: PendingIntentKind) -> &'static str {
    match kind {
        PendingIntentKind::Upload => "upload",
        PendingIntentKind::Delete => "delete",
        PendingIntentKind::Rename => "rename",
        PendingIntentKind::Download => "download",
        PendingIntentKind::ApplyRemoteDelete => "apply_remote_delete",
        PendingIntentKind::ReconcileSubtree => "reconcile_subtree",
    }
}

fn intent_kind_from_label(label: &str) -> Result<PendingIntentKind, StateDbError> {
    match label {
        "upload" => Ok(PendingIntentKind::Upload),
        "delete" => Ok(PendingIntentKind::Delete),
        "rename" => Ok(PendingIntentKind::Rename),
        "download" => Ok(PendingIntentKind::Download),
        "apply_remote_delete" => Ok(PendingIntentKind::ApplyRemoteDelete),
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
    use std::fs;
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
        assert_eq!(leased.attempt_count, 0);
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
    fn batched_leasing_preserves_ready_order() {
        let temp_dir = TempDir::new().expect("temp dir");
        let database_path = temp_dir.path().join("state/vapor.sqlite");
        let mut database = DurableStateDb::open(&database_path).expect("open durable state db");
        let watch_root = PathBuf::from("/tmp/vapor-root");

        database
            .enqueue_intent(
                &watch_root.join("older.txt"),
                PendingIntentKind::Upload,
                timestamp_ms(100),
            )
            .expect("enqueue older intent");
        database
            .enqueue_intent(
                &watch_root.join("newer.txt"),
                PendingIntentKind::Upload,
                timestamp_ms(200),
            )
            .expect("enqueue newer intent");

        let leased = database
            .lease_ready_batch(timestamp_ms(200), 2)
            .expect("lease ready batch");

        assert_eq!(leased.len(), 2);
        assert_eq!(leased[0].path, watch_root.join("older.txt"));
        assert_eq!(leased[1].path, watch_root.join("newer.txt"));
    }

    #[test]
    fn peek_next_ready_kind_reports_oldest_ready_intent_kind() {
        let temp_dir = TempDir::new().expect("temp dir");
        let database_path = temp_dir.path().join("state/vapor.sqlite");
        let mut database = DurableStateDb::open(&database_path).expect("open durable state db");
        let watch_root = PathBuf::from("/tmp/vapor-root");

        database
            .enqueue_intent(
                &watch_root.join("delete.txt"),
                PendingIntentKind::Delete,
                timestamp_ms(100),
            )
            .expect("enqueue delete intent");
        database
            .enqueue_intent(
                &watch_root.join("upload.txt"),
                PendingIntentKind::Upload,
                timestamp_ms(200),
            )
            .expect("enqueue upload intent");

        assert_eq!(
            database
                .peek_next_ready_kind(timestamp_ms(200))
                .expect("peek next ready kind"),
            Some(PendingIntentKind::Delete)
        );
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
            assert_eq!(leased.attempt_count, 0);
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
        assert_eq!(replayed.attempt_count, 0);
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
        assert_eq!(retried.attempt_count, 0);
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
        assert_eq!(retried.attempt_count, 1);
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
    fn persisted_retry_error_text_is_sanitized_and_bounded() {
        let temp_dir = TempDir::new().expect("temp dir");
        let database_path = temp_dir.path().join("state/vapor.sqlite");
        let mut database = DurableStateDb::open(&database_path).expect("open durable state db");
        let path = PathBuf::from("/tmp/vapor-root/project/file.txt");
        let long_error = format!("Bearer {}", "x".repeat(5_000));

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
                RetryFailureKind::Transient,
                &long_error,
                timestamp_ms(100),
            )
            .expect("schedule retry")
            .expect("scheduled retry");

        let last_error = scheduled.intent.last_error.expect("persisted last_error");
        assert_eq!(last_error, "[REDACTED]");
        assert!(last_error.len() <= constants::state::MAX_DIAGNOSTIC_TEXT_LENGTH);
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
    fn oversized_state_values_are_rejected() {
        let temp_dir = TempDir::new().expect("temp dir");
        let database_path = temp_dir.path().join("state/vapor.sqlite");
        let mut database = DurableStateDb::open(&database_path).expect("open durable state db");
        let oversized = "x".repeat(constants::state::MAX_STATE_VALUE_LENGTH + 1);

        let error = database
            .set_state("queue.resume_marker", &oversized, timestamp_ms(100))
            .expect_err("oversized state value should fail");

        assert!(matches!(error, StateDbError::InvalidStateValue(_)));
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
        assert_eq!(retried.attempt_count, 1);
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
    fn non_utf8_paths_are_rejected_when_enqueued() {
        // Pre-GA we standardize on UTF-8 path storage so the durable schema
        // is portable across macOS / Linux / Windows. Non-UTF-8 paths are
        // rejected at the enqueue boundary instead of silently corrupting
        // either the wire format or the consumer side.
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;

            let temp_dir = TempDir::new().expect("temp dir");
            let database_path = temp_dir.path().join("state/vapor.sqlite");
            let mut database = DurableStateDb::open(&database_path).expect("open durable state db");
            let path = PathBuf::from(std::ffi::OsString::from_vec(vec![0x66, 0x6f, 0x80]));

            let error = database
                .enqueue_intent(&path, PendingIntentKind::Upload, timestamp_ms(100))
                .expect_err("non-UTF-8 enqueue should be rejected");

            assert!(matches!(error, StateDbError::NonUtf8Path(_)));
        }
        #[cfg(not(unix))]
        {
            // Windows OS strings can carry unpaired surrogates. The
            // path_to_text helper returns NonUtf8Path in that case as well;
            // the unit test for `path_to_text` covers that branch directly
            // without depending on platform-specific OsString constructors.
        }
    }

    #[test]
    fn path_to_text_returns_utf8_for_ascii_paths() {
        let path = PathBuf::from("/tmp/vapor/file.txt");
        let text = path_to_text(&path).expect("ascii path is UTF-8");
        assert_eq!(text, "/tmp/vapor/file.txt");
    }

    #[cfg(unix)]
    #[test]
    fn path_to_text_rejects_non_utf8_path_unix() {
        use std::os::unix::ffi::OsStringExt;

        let path = PathBuf::from(std::ffi::OsString::from_vec(vec![0x66, 0x6f, 0x80]));
        let error = path_to_text(&path).expect_err("non-UTF-8 path should error");
        assert!(matches!(error, StateDbError::NonUtf8Path(_)));
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
    fn version_two_database_is_rejected_after_pre_ga_path_text_bump() {
        // Schema v2 stored paths as `path_bytes BLOB`. The C1-3 portability
        // bump moved to `path_text TEXT`. Pre-GA we reject the prior schema
        // outright per AGENTS.md §1.1; a future GA migration step would
        // upgrade in place.
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
                 INSERT INTO schema_meta (singleton, schema_version) VALUES (1, 2);
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
                 );",
            )
            .expect("seed version two schema");
        drop(connection);

        let error = DurableStateDb::open(&database_path)
            .expect_err("version two schema should be rejected");
        assert!(matches!(
            error,
            StateDbError::SchemaVersionMismatch {
                found: 2,
                expected: CURRENT_SCHEMA_VERSION
            }
        ));
    }

    #[test]
    fn version_one_database_is_rejected_in_pre_ga_state() {
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

        let error = DurableStateDb::open(&database_path)
            .expect_err("version one schema should be rejected");
        assert!(matches!(
            error,
            StateDbError::SchemaVersionMismatch {
                found: 1,
                expected: CURRENT_SCHEMA_VERSION
            }
        ));
    }

    #[test]
    fn version_three_database_migrates_in_place_to_v4_preserving_rows() {
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
                 INSERT INTO schema_meta (singleton, schema_version) VALUES (1, 3);
                 CREATE TABLE queue_intents (
                     id INTEGER PRIMARY KEY AUTOINCREMENT,
                     path_text TEXT NOT NULL,
                     kind TEXT NOT NULL CHECK(kind IN ('upload', 'delete', 'rename', 'reconcile_subtree')),
                     state TEXT NOT NULL CHECK(state IN ('pending', 'leased')),
                     enqueued_at_ms INTEGER NOT NULL,
                     available_at_ms INTEGER NOT NULL,
                     leased_at_ms INTEGER,
                     attempt_count INTEGER NOT NULL DEFAULT 0 CHECK(attempt_count >= 0),
                     last_error TEXT
                 );
                 CREATE INDEX idx_queue_intents_ready
                     ON queue_intents(state, available_at_ms, id);
                 CREATE TABLE failed_intents (
                     id INTEGER PRIMARY KEY,
                     path_text TEXT NOT NULL,
                     kind TEXT NOT NULL CHECK(kind IN ('upload', 'delete', 'rename', 'reconcile_subtree')),
                     failure_kind TEXT NOT NULL CHECK(failure_kind IN ('authentication', 'permanent')),
                     enqueued_at_ms INTEGER NOT NULL,
                     failed_at_ms INTEGER NOT NULL,
                     attempt_count INTEGER NOT NULL CHECK(attempt_count >= 0),
                     last_error TEXT NOT NULL
                 );
                 CREATE TABLE state_entries (
                     key TEXT PRIMARY KEY,
                     value TEXT NOT NULL,
                     updated_at_ms INTEGER NOT NULL
                 );
                 INSERT INTO queue_intents
                     (path_text, kind, state, enqueued_at_ms, available_at_ms,
                      leased_at_ms, attempt_count, last_error)
                     VALUES ('/tmp/vapor-root/preserved.txt', 'upload', 'pending',
                             100, 100, NULL, 2, 'retry me');",
            )
            .expect("seed version three schema");
        drop(connection);

        let mut migrated = DurableStateDb::open(&database_path)
            .expect("version three database must migrate forward in place");
        // The pre-migration row survived with its retry bookkeeping.
        let preserved = migrated
            .intent_record(1)
            .expect("read preserved row")
            .expect("preserved row exists");
        assert_eq!(
            preserved.path,
            PathBuf::from("/tmp/vapor-root/preserved.txt")
        );
        assert_eq!(preserved.attempt_count, 2);
        // The widened kind vocabulary is accepted post-migration.
        migrated
            .enqueue_intent(
                &PathBuf::from("/tmp/vapor-root/downloaded.txt"),
                PendingIntentKind::Download,
                timestamp_ms(200),
            )
            .expect("v4 kinds must be storable after migration");
    }

    #[test]
    fn corrupt_database_is_quarantined_and_replaced_with_a_fresh_one() {
        let temp_dir = TempDir::new().expect("temp dir");
        let database_path = temp_dir.path().join("state/vapor.sqlite");
        fs::create_dir_all(database_path.parent().unwrap()).expect("parent");
        fs::write(&database_path, b"this is not a sqlite database").expect("seed garbage");

        let mut recovered =
            DurableStateDb::open_with_corruption_recovery(&database_path, timestamp_ms(1_000))
                .expect("corruption must recover, not crash-loop");
        // The fresh database is fully usable.
        recovered
            .enqueue_intent(
                &PathBuf::from("/tmp/vapor-root/after-recovery.txt"),
                PendingIntentKind::Upload,
                timestamp_ms(2_000),
            )
            .expect("fresh database accepts intents");
        // The corrupt payload is preserved for inspection.
        let quarantined: Vec<_> = fs::read_dir(database_path.parent().unwrap())
            .expect("read state dir")
            .filter_map(|entry| entry.ok())
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .contains("sqlite.corrupt-")
            })
            .collect();
        assert_eq!(quarantined.len(), 1, "corrupt file must be quarantined");
    }

    #[test]
    fn schema_mismatch_is_not_treated_as_corruption() {
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
            .expect("seed future schema");
        drop(connection);

        let error =
            DurableStateDb::open_with_corruption_recovery(&database_path, timestamp_ms(1_000))
                .expect_err("future schema must error, not quarantine");
        assert!(matches!(error, StateDbError::SchemaVersionMismatch { .. }));
        assert!(
            database_path.exists(),
            "the database must not be quarantined"
        );
    }

    #[test]
    fn oversized_attempt_count_in_local_state_is_rejected() {
        let temp_dir = TempDir::new().expect("temp dir");
        let database_path = temp_dir.path().join("state/vapor.sqlite");
        let mut database = DurableStateDb::open(&database_path).expect("open durable state db");
        let path = PathBuf::from("/tmp/vapor-root/project/file.txt");
        let queued = database
            .enqueue_intent(&path, PendingIntentKind::Upload, timestamp_ms(100))
            .expect("enqueue upload intent");
        drop(database);

        let connection = Connection::open(&database_path).expect("open sqlite connection");
        configure_connection(&connection).expect("configure connection");
        connection
            .execute(
                "UPDATE queue_intents SET attempt_count = ? WHERE id = ?",
                params![
                    i64::from(constants::state::MAX_ATTEMPT_COUNT) + 1,
                    queued.id
                ],
            )
            .expect("tamper attempt count");
        drop(connection);

        let reopened = DurableStateDb::open(&database_path).expect("reopen durable state db");
        let error = reopened
            .intent_record(queued.id)
            .expect_err("oversized attempt count should fail");
        assert!(matches!(error, StateDbError::InvalidAttemptCount(_)));
    }

    #[test]
    fn coalesced_enqueue_skips_paths_with_an_existing_pending_row() {
        let temp_dir = TempDir::new().expect("temp dir");
        let database_path = temp_dir.path().join("state/vapor.sqlite");
        let mut database = DurableStateDb::open(&database_path).expect("open durable state db");
        let path = PathBuf::from("/tmp/vapor-root/project/file.txt");

        let inserted = database
            .enqueue_intents_coalesced(&[
                (path.clone(), PendingIntentKind::Upload, timestamp_ms(100)),
                (path.clone(), PendingIntentKind::Upload, timestamp_ms(200)),
            ])
            .expect("coalesced enqueue");
        assert_eq!(inserted, 1);
        assert_eq!(database.pending_depth().expect("pending depth"), 1);

        // A second batch for the same still-pending path coalesces too.
        let inserted = database
            .enqueue_intents_coalesced(&[(
                path.clone(),
                PendingIntentKind::Upload,
                timestamp_ms(300),
            )])
            .expect("coalesced enqueue");
        assert_eq!(inserted, 0);

        // A different kind for the same path is separate work.
        let inserted = database
            .enqueue_intents_coalesced(&[(
                path.clone(),
                PendingIntentKind::Delete,
                timestamp_ms(400),
            )])
            .expect("coalesced enqueue");
        assert_eq!(inserted, 1);
        assert_eq!(database.pending_depth().expect("pending depth"), 2);
    }

    #[test]
    fn coalesced_enqueue_does_not_coalesce_against_leased_rows() {
        let temp_dir = TempDir::new().expect("temp dir");
        let database_path = temp_dir.path().join("state/vapor.sqlite");
        let mut database = DurableStateDb::open(&database_path).expect("open durable state db");
        let path = PathBuf::from("/tmp/vapor-root/project/file.txt");

        database
            .enqueue_intent(&path, PendingIntentKind::Upload, timestamp_ms(100))
            .expect("enqueue upload intent");
        database
            .lease_next_ready(timestamp_ms(100))
            .expect("lease next ready")
            .expect("leased record");

        // The in-flight lease may have read stale content; the new change
        // must survive as its own pending row.
        let inserted = database
            .enqueue_intents_coalesced(&[(
                path.clone(),
                PendingIntentKind::Upload,
                timestamp_ms(200),
            )])
            .expect("coalesced enqueue");
        assert_eq!(inserted, 1);
        assert_eq!(database.pending_depth().expect("pending depth"), 1);
        assert_eq!(database.leased_depth().expect("leased depth"), 1);
    }

    #[test]
    fn in_run_stale_lease_sweep_recovers_only_stale_leases() {
        let temp_dir = TempDir::new().expect("temp dir");
        let database_path = temp_dir.path().join("state/vapor.sqlite");
        let mut database = DurableStateDb::open(&database_path).expect("open durable state db");
        let stale_path = PathBuf::from("/tmp/vapor-root/stale.txt");
        let fresh_path = PathBuf::from("/tmp/vapor-root/fresh.txt");

        database
            .enqueue_intent(&stale_path, PendingIntentKind::Upload, timestamp_ms(0))
            .expect("enqueue stale intent");
        database
            .lease_next_ready(timestamp_ms(0))
            .expect("lease stale")
            .expect("stale lease");

        let lease_timeout = constants::engine::LEASE_TIMEOUT_MILLIS;
        database
            .enqueue_intent(
                &fresh_path,
                PendingIntentKind::Upload,
                timestamp_ms(lease_timeout + 1_000),
            )
            .expect("enqueue fresh intent");
        database
            .lease_next_ready(timestamp_ms(lease_timeout + 1_000))
            .expect("lease fresh")
            .expect("fresh lease");

        let recovered = database
            .recover_stale_leases(timestamp_ms(lease_timeout + 2_000))
            .expect("stale sweep");

        assert_eq!(recovered, 1);
        assert_eq!(database.leased_depth().expect("leased depth"), 1);
        assert_eq!(database.pending_depth().expect("pending depth"), 1);
        let replayed = database
            .lease_next_ready(timestamp_ms(lease_timeout + 3_000))
            .expect("lease replayed")
            .expect("replayed record");
        assert_eq!(replayed.path, stale_path);
    }

    fn timestamp_ms(milliseconds: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_millis(milliseconds)
    }
}
