use std::error::Error;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use vapor_shared::{constants, logging::sanitize_diagnostic_text, runtime_paths};

use crate::event_intents::PendingIntentKind;
use crate::retry::{RetryDecision, RetryFailureKind, RetryPolicy};

/// Before GA there is no migration: a database of any other version is
/// set aside by the daemon and rebuilt from a whole-scope reconcile.
const CURRENT_SCHEMA_VERSION: i64 = 8;
/// How long a database that was set aside (corrupt, or of another
/// schema version) stays on disk for inspection.
const QUARANTINE_RETENTION: Duration = Duration::from_secs(30 * 24 * 60 * 60);
const STATE_PENDING: &str = "pending";
const STATE_LEASED: &str = "leased";
/// An intent parked behind a pending decision: never leased until the
/// decision is resolved, then released or dropped by its answer.
const STATE_HELD: &str = "held";
/// Candidate conflict copies: the marker match is what the partial
/// indexes cover; the strict name grammar is applied in Rust so a user
/// file that merely contains the marker text is not counted. The
/// `LIKE` pattern is inlined (not bound) because the planner only uses
/// a partial index when the query's predicate is the index's own.
const UNRESOLVED_CONFLICT_COPY_QUERY: &str =
    "SELECT path_text FROM sync_index WHERE path_text LIKE '%~conflict-%'
     UNION
     SELECT path_text FROM queue_intents WHERE path_text LIKE '%~conflict-%' AND kind = 'upload'";

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

/// What a decision of the user allows an intent to do.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Approval {
    #[default]
    None,
    /// The guard that held the intent lets it pass. Nothing else
    /// changes: a deletion still needs its sync index row.
    Guards,
    /// The user's answer named this very path (the side of a clash to
    /// remove). It passes the guards, and for a deletion it stands in
    /// for the sync index row.
    NamedPath,
}

impl Approval {
    /// The value of the `approved` column.
    fn stored(self) -> i64 {
        match self {
            Approval::None => 0,
            Approval::Guards => 1,
            Approval::NamedPath => 2,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DurableIntentRecord {
    pub id: i64,
    pub path: PathBuf,
    pub kind: PendingIntentKind,
    /// Durable lease-order class (lower leases first); see
    /// [`crate::safeguards::durable_intent_priority_rank`].
    pub priority_rank: u8,
    pub enqueued_at: SystemTime,
    pub available_at: SystemTime,
    pub leased_at: Option<SystemTime>,
    pub attempt_count: u32,
    pub last_error: Option<String>,
    /// For a download whose remote object does not live at the local
    /// path's mirror (a name that collides on this filesystem,
    /// materialized as a conflict copy): the remote path to fetch.
    pub remote_path: Option<String>,
    /// The user approved this intent through a decision; the guards
    /// that hold deletions let it through.
    pub approved: bool,
    /// The user's answer named this path. For a deletion, the answer
    /// then stands in for the sync index row that it has not got.
    pub names_path: bool,
    /// The decision this intent is parked behind, while held.
    pub held_by: Option<i64>,
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
    /// Folders that the index holds under one spelling only, as the
    /// last look found them ([`Self::sync_index_row_spelled_otherwise`]).
    /// The look for another spelling reads every index entry under a
    /// folder when there is none, and the removal of a folder is
    /// planned again each time it has to wait. Only a write that adds a
    /// path to the index can add a spelling, so each such write empties
    /// the set.
    one_spelling: std::cell::RefCell<std::collections::HashSet<PathBuf>>,
    /// A batch is open ([`Self::begin_batch`]): every write since then
    /// is part of one transaction, which [`Self::commit_batch`] ends.
    batch_open: bool,
}

/// One write of several statements that lands whole or not at all: a
/// transaction of its own, or a savepoint inside the batch that is
/// open. Dropped without a commit, it is taken back.
enum Atomic<'connection> {
    Transaction(rusqlite::Transaction<'connection>),
    Savepoint(rusqlite::Savepoint<'connection>),
}

impl<'connection> Atomic<'connection> {
    fn begin(connection: &'connection mut Connection, in_batch: bool) -> rusqlite::Result<Self> {
        if in_batch {
            connection.savepoint().map(Self::Savepoint)
        } else {
            connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map(Self::Transaction)
        }
    }

    fn commit(self) -> rusqlite::Result<()> {
        match self {
            Self::Transaction(transaction) => transaction.commit(),
            Self::Savepoint(savepoint) => savepoint.commit(),
        }
    }
}

impl std::ops::Deref for Atomic<'_> {
    type Target = Connection;

    fn deref(&self) -> &Connection {
        match self {
            Self::Transaction(transaction) => transaction,
            Self::Savepoint(savepoint) => savepoint,
        }
    }
}

/// The most folders [`DurableStateDb::one_spelling`] remembers.
const ONE_SPELLING_MEMO_LIMIT: usize = 4_096;

impl DurableStateDb {
    pub fn open_default() -> Result<Self, StateDbError> {
        Self::open(runtime_paths::sqlite_database_path())
    }

    /// The daemon's open. A file that cannot serve as the state
    /// database is set aside next to itself and a fresh database takes
    /// its place: a file that is not a readable SQLite database or
    /// fails the page check (`vapor.sqlite.corrupt-<ms>`), and a
    /// database of another schema version
    /// (`vapor.sqlite.schema-v<n>-<ms>`). The startup whole-scope
    /// reconcile rebuilds the index, and with no index row nothing is
    /// deleted on either side. Files set aside stay for the retention
    /// window, then go.
    pub fn open_with_corruption_recovery(
        path: impl AsRef<Path>,
        now: SystemTime,
    ) -> Result<Self, StateDbError> {
        let path = path.as_ref().to_path_buf();
        prune_quarantined(&path, now);
        // Only a file that can never serve may be set aside. Transient
        // failures (disk full, I/O errors, busy locks, permissions)
        // must not replace a healthy database full of pending intent
        // state; they surface as ordinary startup failures so the
        // crash-loop guard retries instead.
        let reason = match Self::open(&path) {
            Ok(database) => match database.damaged_pages() {
                Ok(None) => return Ok(database),
                Ok(Some(finding)) => QuarantineReason::Corrupt(finding),
                Err(StateDbError::Sql(error)) if is_corruption_error(&error) => {
                    QuarantineReason::Corrupt(error.to_string())
                }
                Err(error) => return Err(error),
            },
            Err(StateDbError::Sql(error)) if is_corruption_error(&error) => {
                QuarantineReason::Corrupt(error.to_string())
            }
            Err(StateDbError::SchemaVersionMismatch { found, .. }) => {
                QuarantineReason::OtherSchema(found)
            }
            Err(error) => return Err(error),
        };
        let now_ms = system_time_to_millis(now).unwrap_or(0);
        let quarantine = match &reason {
            QuarantineReason::Corrupt(_) => path.with_extension(format!("sqlite.corrupt-{now_ms}")),
            QuarantineReason::OtherSchema(found) => {
                path.with_extension(format!("sqlite.schema-v{found}-{now_ms}"))
            }
        };
        let metadata = [
            ("database_path", path.display().to_string()),
            ("quarantine_path", quarantine.display().to_string()),
        ];
        match &reason {
            QuarantineReason::Corrupt(finding) => crate::logging::error(
                "Durable state DB is corrupt; setting it aside and starting fresh",
                &[
                    metadata[0].clone(),
                    metadata[1].clone(),
                    ("error", finding.clone()),
                ],
            ),
            QuarantineReason::OtherSchema(found) => crate::logging::info(
                "Durable state DB has another schema version; setting it aside and rebuilding from a whole-scope reconcile",
                &[
                    metadata[0].clone(),
                    metadata[1].clone(),
                    ("found", found.to_string()),
                    ("expected", CURRENT_SCHEMA_VERSION.to_string()),
                ],
            ),
        }
        std::fs::rename(&path, &quarantine)?;
        // WAL/SHM siblings belong to the database that was set aside.
        for suffix in ["-wal", "-shm"] {
            let mut sibling = path.clone().into_os_string();
            sibling.push(suffix);
            let _ = std::fs::remove_file(std::path::PathBuf::from(sibling));
        }
        Self::open(&path)
    }

    /// The first finding of SQLite's page check, or `None` for a sound
    /// file. Run once per daemon start: a damaged page inside a table
    /// otherwise fails later, at run time, on every start.
    fn damaged_pages(&self) -> Result<Option<String>, StateDbError> {
        let finding: String = self
            .connection
            .query_row("PRAGMA quick_check(1)", [], |row| row.get(0))?;
        Ok((finding != "ok").then_some(finding))
    }

    /// Opens the database for a caller that does not own it (the CLI).
    /// A database of another schema version is an error here, never
    /// set aside.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StateDbError> {
        let path = path.as_ref().to_path_buf();
        runtime_paths::ensure_private_file(&path)?;

        let mut connection = Connection::open(&path)?;
        runtime_paths::ensure_private_file(&path)?;
        configure_connection(&mut connection)?;
        ensure_schema(&mut connection)?;

        Ok(Self {
            path,
            connection,
            one_spelling: Default::default(),
            batch_open: false,
        })
    }

    /// A private, process-local database: the inert runtime a parked
    /// profile is composed with when its own state DB cannot be read.
    pub fn open_in_memory() -> Result<Self, StateDbError> {
        let mut connection = Connection::open_in_memory()?;
        configure_connection(&mut connection)?;
        ensure_schema(&mut connection)?;
        Ok(Self {
            path: PathBuf::from(":memory:"),
            connection,
            one_spelling: Default::default(),
            batch_open: false,
        })
    }

    /// Starts a batch: every write until [`Self::commit_batch`] is one
    /// transaction, and so one write to the log of the database. The
    /// runtime opens one for each tick. A burst of small files costs
    /// about three writes for each file; each on its own was a write to
    /// the log, which on macOS waits behind the flush of the drive that
    /// the transfers of the same moment ask for.
    ///
    /// A daemon that is killed inside a batch loses the writes of that
    /// batch and nothing else: the intents of that tick are leased or
    /// pending again at the next start, and each one is planned against
    /// what the two sides hold then.
    pub fn begin_batch(&mut self) -> Result<(), StateDbError> {
        if !self.batch_open {
            self.connection.execute_batch("BEGIN IMMEDIATE")?;
            self.batch_open = true;
        }
        Ok(())
    }

    /// Ends the batch that is open, when there is one.
    pub fn commit_batch(&mut self) -> Result<(), StateDbError> {
        if self.batch_open {
            self.batch_open = false;
            self.connection.execute_batch("COMMIT")?;
        }
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn schema_version(&self) -> Result<i64, StateDbError> {
        read_schema_version(&self.connection)?.ok_or(StateDbError::MissingSchemaVersion)
    }

    /// The first `limit` queue rows (pending and leased) in lease order,
    /// so the diagnostics surface shows the queue the way the executor
    /// will drain it.
    pub fn list_queue_intents(
        &self,
        limit: usize,
    ) -> Result<Vec<DurableIntentRecord>, StateDbError> {
        let mut statement = self.connection.prepare(&format!(
            "SELECT {INTENT_COLUMNS}
             FROM queue_intents
             ORDER BY priority_rank ASC, available_at_ms ASC, id ASC
             LIMIT ?"
        ))?;
        let rows = statement.query_map(
            params![i64::try_from(limit).unwrap_or(i64::MAX)],
            intent_from_row,
        )?;
        let mut records = Vec::new();
        for row in rows {
            records.push(raw_intent_to_record(row?)?);
        }
        Ok(records)
    }

    /// Intents waiting to be worked (pending or leased). Held intents
    /// are parked behind a decision, not queued work; `held_intent_count`
    /// reports them.
    pub fn queue_depth(&self) -> Result<usize, StateDbError> {
        count_intents(&self.connection, None)
    }

    pub fn pending_depth(&self) -> Result<usize, StateDbError> {
        count_intents(&self.connection, Some(STATE_PENDING))
    }

    pub fn leased_depth(&self) -> Result<usize, StateDbError> {
        count_intents(&self.connection, Some(STATE_LEASED))
    }

    /// Number of queued intents (pending or leased) of `kind`, excluding
    /// `except_id`. The deletion guard reads it to judge a burst as a
    /// whole before the first deletion lands.
    pub fn queued_deletions(
        &self,
        kind: PendingIntentKind,
        except_id: i64,
    ) -> Result<usize, StateDbError> {
        let count = self.connection.query_row(
            "SELECT COUNT(*) FROM queue_intents
             WHERE kind = ? AND id != ? AND state IN (?, ?)",
            params![
                intent_kind_label(kind),
                except_id,
                STATE_PENDING,
                STATE_LEASED
            ],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(count as usize)
    }

    /// Paths of the queued intents (pending or leased) of `kind`.
    pub fn queued_paths_of_kind(
        &self,
        kind: PendingIntentKind,
    ) -> Result<Vec<PathBuf>, StateDbError> {
        let mut statement = self.connection.prepare(
            // The newest rows: the create half of a rename is queued
            // within moments of the deletion that asks.
            "SELECT path_text FROM queue_intents WHERE kind = ? AND state IN (?, ?)
             ORDER BY id DESC LIMIT 256",
        )?;
        let rows = statement.query_map(
            params![intent_kind_label(kind), STATE_PENDING, STATE_LEASED],
            |row| row.get::<_, String>(0),
        )?;
        let mut paths = Vec::new();
        for row in rows {
            paths.push(path_from_text(row?));
        }
        Ok(paths)
    }

    /// Kinds of the queued intents (pending or leased) at exactly
    /// `path`. The move detector reads it: a path with a restore
    /// queued is being put back, not renamed away.
    pub fn queued_kinds_at(&self, path: &Path) -> Result<Vec<PendingIntentKind>, StateDbError> {
        let mut statement = self
            .connection
            .prepare("SELECT kind FROM queue_intents WHERE path_text = ? AND state IN (?, ?)")?;
        let rows = statement.query_map(
            params![path_to_text(path)?, STATE_PENDING, STATE_LEASED],
            |row| row.get::<_, String>(0),
        )?;
        let mut kinds = Vec::new();
        for row in rows {
            kinds.push(intent_kind_from_label(&row?)?);
        }
        Ok(kinds)
    }

    /// Number of queued intents (pending or leased) whose path lies
    /// strictly under `directory`, excluding `except_id`. A directory
    /// delete uses it to tell "children still being worked on" from
    /// "children kept on purpose".
    pub fn intents_under(&self, directory: &Path, except_id: i64) -> Result<usize, StateDbError> {
        let mut prefix = path_to_text(directory)?;
        if !prefix.ends_with(std::path::MAIN_SEPARATOR) {
            prefix.push(std::path::MAIN_SEPARATOR);
        }
        // LIKE would treat `_` and `%` in the prefix as wildcards; a
        // range comparison on the text column is exact.
        let mut upper = prefix.clone();
        upper.push(char::MAX);
        let count = self.connection.query_row(
            "SELECT COUNT(*) FROM queue_intents
             WHERE path_text > ? AND path_text < ? AND id != ?",
            params![prefix, upper, except_id],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(count as usize)
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
        let transaction = Atomic::begin(&mut self.connection, self.batch_open)?;
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
                crate::safeguards::IntentSource::Fresh,
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

    /// One line for each failed intent (kind, path, error), for the
    /// message of a test that expects none.
    #[cfg(test)]
    pub fn failed_summaries(&self) -> Vec<String> {
        let Ok(mut statement) = self
            .connection
            .prepare("SELECT id FROM failed_intents ORDER BY id")
        else {
            return Vec::new();
        };
        let ids: Vec<i64> = statement
            .query_map([], |row| row.get(0))
            .map(|rows| rows.flatten().collect())
            .unwrap_or_default();
        ids.into_iter()
            .filter_map(|id| self.failed_record(id).ok().flatten())
            .map(|record| format!("{record:?}"))
            .collect()
    }

    /// One line for each queued intent (kind, path, attempts, last
    /// error), for the message of a test that expects an empty queue.
    #[cfg(test)]
    pub fn queued_summaries(&self) -> Vec<String> {
        let Ok(mut statement) = self
            .connection
            .prepare("SELECT id FROM queue_intents ORDER BY id")
        else {
            return Vec::new();
        };
        let ids: Vec<i64> = statement
            .query_map([], |row| row.get(0))
            .map(|rows| rows.flatten().collect())
            .unwrap_or_default();
        ids.into_iter()
            .filter_map(|id| self.intent_record(id).ok().flatten())
            .map(|record| {
                format!(
                    "{:?} {} attempts={} last_error={:?} held_by={:?}",
                    record.kind,
                    record.path.display(),
                    record.attempt_count,
                    record.last_error,
                    record.held_by
                )
            })
            .collect()
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
             ORDER BY priority_rank ASC, available_at_ms ASC, id ASC
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
        let mut statement = self.connection.prepare(&format!(
            "UPDATE queue_intents
             SET state = ?, leased_at_ms = ?
             WHERE id IN (
                 SELECT id FROM queue_intents
                 WHERE state = ? AND available_at_ms <= ?
                 ORDER BY priority_rank ASC, available_at_ms ASC, id ASC
                 LIMIT ?
             )
             RETURNING {INTENT_COLUMNS}"
        ))?;
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

        leased_intents.sort_by_key(|intent| (intent.priority_rank, intent.available_at, intent.id));
        Ok(leased_intents)
    }

    pub fn complete_leased(&mut self, id: i64) -> Result<bool, StateDbError> {
        let changed = self.connection.execute(
            "DELETE FROM queue_intents WHERE id = ? AND state = ?",
            params![id, STATE_LEASED],
        )?;
        Ok(changed > 0)
    }

    /// Puts a leased intent back for `available_at` because the
    /// planner chose to wait (a deletion that may be half of a rename),
    /// counting the deferral so it cannot repeat forever and recording
    /// the reason where diagnostics show it.
    pub fn defer_leased(
        &mut self,
        id: i64,
        available_at: SystemTime,
        reason: &str,
    ) -> Result<bool, StateDbError> {
        let available_at_ms = system_time_to_millis(available_at)?;
        let changed = self.connection.execute(
            "UPDATE queue_intents
             SET state = ?, available_at_ms = ?, leased_at_ms = NULL,
                 attempt_count = attempt_count + 1, last_error = ?
             WHERE id = ? AND state = ?",
            params![
                STATE_PENDING,
                available_at_ms,
                sanitize_persisted_error(reason),
                id,
                STATE_LEASED
            ],
        )?;
        Ok(changed > 0)
    }

    /// Makes every retry at `path` or under it due now: the user gave
    /// an answer about that path, and the work it stopped goes on.
    pub fn wake_retries_under(
        &mut self,
        path: &Path,
        now: SystemTime,
    ) -> Result<usize, StateDbError> {
        let (lower, upper) = rows_under_bounds(path)?;
        Ok(self.connection.execute(
            &format!(
                "UPDATE queue_intents SET available_at_ms = ?1
                 WHERE (path_text = ?2 OR (path_text >= ?3 AND path_text < ?4)) AND {}",
                waiting_retry_predicate()
            ),
            params![
                system_time_to_millis(now)?,
                path_to_text(path)?,
                lower,
                upper
            ],
        )?)
    }

    /// Makes every retry due now: the user asked for a sync.
    pub fn wake_all_retries(&mut self, now: SystemTime) -> Result<usize, StateDbError> {
        Ok(self.connection.execute(
            &format!(
                "UPDATE queue_intents SET available_at_ms = ?1 WHERE {}",
                waiting_retry_predicate()
            ),
            params![system_time_to_millis(now)?],
        )?)
    }

    /// Queued intents that keep failing: at least
    /// `BLOCKED_AFTER_ATTEMPTS` tries, with the reason of the last one.
    /// They are not work in progress, and the status counts them apart.
    pub fn blocked_depth(&self) -> Result<usize, StateDbError> {
        let count: i64 = self.connection.query_row(
            &format!(
                "SELECT COUNT(*) FROM queue_intents WHERE {}",
                blocked_predicate()
            ),
            [],
            |row| row.get(0),
        )?;
        Ok(usize::try_from(count).unwrap_or(0))
    }

    /// The first `limit` blocked intents, oldest first.
    pub fn blocked_intents(&self, limit: usize) -> Result<Vec<DurableIntentRecord>, StateDbError> {
        let mut statement = self.connection.prepare(&format!(
            "SELECT id FROM queue_intents WHERE {} ORDER BY id LIMIT ?",
            blocked_predicate()
        ))?;
        let ids: Vec<i64> = statement
            .query_map(params![i64::try_from(limit).unwrap_or(i64::MAX)], |row| {
                row.get(0)
            })?
            .collect::<Result<_, _>>()?;
        let mut records = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(record) = self.intent_record(id)? {
                records.push(record);
            }
        }
        Ok(records)
    }

    /// Ends the wait of every upload that waits for room in the cloud:
    /// each one that is due later than `now` is due now. Returns how
    /// many were waiting. The stored error is what marks such a row,
    /// so a row that failed for another reason since keeps its wait.
    pub fn end_wait_for_cloud_room(&mut self, now: SystemTime) -> Result<usize, StateDbError> {
        let now_ms = system_time_to_millis(now)?;
        Ok(self.connection.execute(
            "UPDATE queue_intents SET available_at_ms = ?
             WHERE state = ? AND available_at_ms > ?
               AND substr(last_error, 1, ?) = ?",
            params![
                now_ms,
                STATE_PENDING,
                now_ms,
                constants::engine::CLOUD_FULL_WAIT_PREFIX.len() as i64,
                constants::engine::CLOUD_FULL_WAIT_PREFIX
            ],
        )?)
    }

    /// The provider's message of one upload that waits for room in the
    /// cloud, when there is one: a daemon that starts reads from the
    /// queue that the cloud was full.
    pub fn wait_for_cloud_room_message(&self) -> Result<Option<String>, StateDbError> {
        let prefix = constants::engine::CLOUD_FULL_WAIT_PREFIX;
        let row: Option<String> = self
            .connection
            .query_row(
                "SELECT last_error FROM queue_intents
                 WHERE state = ? AND substr(last_error, 1, ?) = ?
                 ORDER BY id LIMIT 1",
                params![STATE_PENDING, prefix.len() as i64, prefix],
                |row| row.get(0),
            )
            .optional()?;
        Ok(row.map(|text| text[prefix.len()..].to_string()))
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
        let available_at_ms = system_time_to_millis(available_at)?;

        // One atomic transaction over the requeue and the slowdown marker:
        // a crash between the two must never persist the retried intent
        // while dropping the durable rate-limit slowdown (which would let
        // other work resume at full speed against a provider that just
        // rate-limited us).
        let slowdown_value = if let Some(slowdown_until) = decision.slowdown_until {
            let persisted = self
                .retry_slowdown_until()?
                .map(|existing| existing.max(slowdown_until))
                .unwrap_or(slowdown_until);
            Some(system_time_to_millis(persisted)?)
        } else {
            None
        };

        let transaction = Atomic::begin(&mut self.connection, self.batch_open)?;
        let changed = transaction.execute(
            "UPDATE queue_intents
             SET state = ?,
                 available_at_ms = ?,
                 leased_at_ms = NULL,
                 last_error = ?,
                 attempt_count = attempt_count + 1,
                 last_attempt_at_ms = ?
             WHERE id = ? AND state = ?",
            params![
                STATE_PENDING,
                available_at_ms,
                Some(sanitize_persisted_error(last_error)),
                system_time_to_millis(now)?,
                id,
                STATE_LEASED
            ],
        )?;
        if changed == 0 {
            return Err(StateDbError::InvalidIntentState(format!(
                "intent {id} is not currently leased"
            )));
        }
        if let Some(slowdown_ms) = slowdown_value {
            let key = constants::state::RETRY_SLOWDOWN_UNTIL_KEY;
            validate_state_key(key)?;
            let value = slowdown_ms.to_string();
            validate_state_value(&value)?;
            transaction.execute(
                "INSERT INTO state_entries (key, value, updated_at_ms)
                 VALUES (?, ?, ?)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at_ms = excluded.updated_at_ms",
                params![key, value, system_time_to_millis(now)?],
            )?;
        }
        transaction.commit()?;

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
        let transaction = Atomic::begin(&mut self.connection, self.batch_open)?;
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
    /// `attempt_count`: documented in `data-flow.md` §Local to remote,
    /// while fresh leases keep their retry history.
    pub fn recover_leased(&mut self, now: SystemTime) -> Result<usize, StateDbError> {
        // Startup: stale leases reset their retry history (a lost process
        // owned them long enough that the backoff should restart).
        let stale_leases_count = self.recover_stale_leases_inner(now, true)?;
        let now_ms = system_time_to_millis(now)?;
        let fresh_leases_count = self.connection.execute(
            "UPDATE queue_intents
             SET state = ?, available_at_ms = ?, leased_at_ms = NULL
             WHERE state = ?",
            params![STATE_PENDING, now_ms, STATE_LEASED],
        )?;
        Ok(stale_leases_count + fresh_leases_count)
    }

    /// In-run recovery sweep: returns leases older than the lease timeout
    /// to `pending`. Only genuinely orphaned leases (a lost execution)
    /// should reach this: the executor renews the lease of every intent
    /// it still holds via [`renew_leases`], so a long transfer or a
    /// Suspended stall no longer trips the sweep. Retry history is kept
    /// (an in-run recovery is not a fresh start).
    pub fn recover_stale_leases(&mut self, now: SystemTime) -> Result<usize, StateDbError> {
        self.recover_stale_leases_inner(now, false)
    }

    fn recover_stale_leases_inner(
        &mut self,
        now: SystemTime,
        reset_attempt_count: bool,
    ) -> Result<usize, StateDbError> {
        let now_ms = system_time_to_millis(now)?;
        let stale_lease_cutoff_ms = now_ms
            .saturating_sub(i64::try_from(constants::engine::LEASE_TIMEOUT_MILLIS).unwrap_or(0));
        let attempt_clause = if reset_attempt_count {
            "attempt_count = 0,"
        } else {
            ""
        };
        let stale_leases_count = self.connection.execute(
            &format!(
                "UPDATE queue_intents
                 SET state = ?,
                     available_at_ms = ?,
                     leased_at_ms = NULL,
                     {attempt_clause}
                     last_error = ?
                 WHERE state = ?
                   AND leased_at_ms IS NOT NULL
                   AND leased_at_ms <= ?"
            ),
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

    /// Renews the lease timestamp of the given leased intents so the
    /// in-run stale-lease sweep does not reclaim work still executing.
    /// No-op for ids that are no longer leased.
    pub fn renew_leases(&mut self, ids: &[i64], now: SystemTime) -> Result<(), StateDbError> {
        if ids.is_empty() {
            return Ok(());
        }
        let now_ms = system_time_to_millis(now)?;
        let placeholders = std::iter::repeat_n("?", ids.len())
            .collect::<Vec<_>>()
            .join(",");
        let mut sql_params: Vec<Box<dyn rusqlite::ToSql>> = Vec::with_capacity(ids.len() + 2);
        sql_params.push(Box::new(now_ms));
        sql_params.push(Box::new(STATE_LEASED));
        for id in ids {
            sql_params.push(Box::new(*id));
        }
        let param_refs: Vec<&dyn rusqlite::ToSql> = sql_params.iter().map(AsRef::as_ref).collect();
        self.connection.execute(
            &format!(
                "UPDATE queue_intents SET leased_at_ms = ?
                 WHERE state = ? AND id IN ({placeholders})"
            ),
            param_refs.as_slice(),
        )?;
        Ok(())
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
        let id = insert_intent(
            &self.connection,
            path,
            kind,
            enqueued_at,
            available_at,
            crate::safeguards::IntentSource::Fresh,
        )?;
        self.intent_record(id)?
            .ok_or(StateDbError::MissingIntentRecord(id))
    }

    /// Enqueues a batch of intents in one transaction, coalescing each
    /// against an existing **pending** row with the same `(path, kind)`:
    /// a path that is already queued (including one waiting out a retry
    /// backoff) does not grow a duplicate row. Leased rows do not
    /// coalesce: work already in flight may have read stale content, so
    /// a fresh pending row is the correct "run again after" signal.
    ///
    /// `source` sets the durable lease priority: reconcile-walk backlog
    /// ranks below fresh work. Coalescing keeps the better (lower) rank,
    /// so a fresh edit landing on a path already queued by a reconcile
    /// promotes that row out of the backlog class instead of inheriting
    /// its starvation.
    ///
    /// A row that is held behind a decision coalesces too: nothing is
    /// inserted next to it.
    ///
    /// Returns the number of rows actually inserted.
    pub fn enqueue_intents_coalesced(
        &mut self,
        intents: &[(PathBuf, PendingIntentKind, SystemTime)],
        source: crate::safeguards::IntentSource,
    ) -> Result<usize, StateDbError> {
        self.enqueue_intents_coalesced_with(intents, source, Approval::None)
    }

    /// Like [`Self::enqueue_intents_coalesced`], with the approval set
    /// on every row it inserts: the intent carries out a decision the
    /// user made (a restore after `discard`, the merge after a root
    /// answer, the side of a clash the user chose to remove). A row
    /// that already existed keeps its own approval.
    pub fn enqueue_intents_coalesced_with(
        &mut self,
        intents: &[(PathBuf, PendingIntentKind, SystemTime)],
        source: crate::safeguards::IntentSource,
        approval: Approval,
    ) -> Result<usize, StateDbError> {
        if intents.is_empty() {
            return Ok(0);
        }

        let transaction = Atomic::begin(&mut self.connection, self.batch_open)?;
        let mut inserted = 0usize;
        for (path, kind, observed_at) in intents {
            let path_text = path_to_text(path)?;
            // A row held behind a decision stands for every later
            // intent of its path and kind: the question that holds it
            // covers them, and a scan that finds the same deletion
            // again adds nothing. An intent that carries the user's
            // own answer is not covered by an older question.
            if approval == Approval::None
                && transaction
                    .query_row(
                        "SELECT 1 FROM queue_intents
                         WHERE path_text = ? AND kind = ? AND state = ?
                         LIMIT 1",
                        params![path_text, intent_kind_label(*kind), STATE_HELD],
                        |_| Ok(()),
                    )
                    .optional()?
                    .is_some()
            {
                continue;
            }
            let existing_pending = transaction
                .query_row(
                    "SELECT id, priority_rank FROM queue_intents
                     WHERE path_text = ? AND kind = ? AND state = ?
                     LIMIT 1",
                    params![path_text, intent_kind_label(*kind), STATE_PENDING],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
                )
                .optional()?;
            if let Some((existing_id, existing_rank)) = existing_pending {
                if source == crate::safeguards::IntentSource::Fresh {
                    wake_retries_of(&transaction, &path_text, *observed_at)?;
                }
                let new_rank = i64::from(crate::safeguards::durable_intent_priority_rank(
                    path, *kind, source,
                ));
                if new_rank < existing_rank {
                    transaction.execute(
                        "UPDATE queue_intents SET priority_rank = ? WHERE id = ?",
                        params![new_rank, existing_id],
                    )?;
                }
                continue;
            }
            if source == crate::safeguards::IntentSource::Fresh {
                wake_retries_of(&transaction, &path_text, *observed_at)?;
            }
            let id = insert_intent(
                &transaction,
                path,
                *kind,
                *observed_at,
                *observed_at,
                source,
            )?;
            if approval != Approval::None {
                transaction.execute(
                    "UPDATE queue_intents SET approved = ? WHERE id = ?",
                    params![approval.stored(), id],
                )?;
            }
            inserted += 1;
        }
        transaction.commit()?;
        Ok(inserted)
    }
}

/// Per-path last-synced state. One row per path that has
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
    /// The remote object's mtime as the provider reported it when the
    /// transfer completed; the same quick check for the cloud side.
    pub remote_modified_at: Option<SystemTime>,
    pub last_op_id: String,
    pub updated_at: SystemTime,
}

impl SyncIndexEntry {
    /// rsync-style quick check: the local file still has the size and
    /// the mtime the index recorded at the last transfer, so its content
    /// has not been touched since. Mtimes compare at the millisecond the
    /// index stores; a filesystem mtime carries nanoseconds, and a plain
    /// equality on the raw value never matched, which silently turned
    /// every quick check into a full hash. An index row without an mtime
    /// never matches, so the caller falls through to hashing.
    pub fn matches_local(&self, size_bytes: u64, modified_at: Option<SystemTime>) -> bool {
        Self::quick_check(
            self.local_modified_at,
            self.size_bytes,
            size_bytes,
            modified_at,
        )
    }

    /// The same quick check against the remote object: its size and
    /// mtime are what the index recorded at the last transfer, so the
    /// cloud copy has not been touched since. An index row without a
    /// remote mtime (written before the provider reported one) never
    /// matches, so the caller falls through to hashing.
    pub fn matches_remote(&self, size_bytes: u64, modified_at: SystemTime) -> bool {
        Self::quick_check(
            self.remote_modified_at,
            self.size_bytes,
            size_bytes,
            Some(modified_at),
        )
    }

    fn quick_check(
        indexed: Option<SystemTime>,
        indexed_size: u64,
        size_bytes: u64,
        modified_at: Option<SystemTime>,
    ) -> bool {
        let Some(indexed) = indexed else {
            return false;
        };
        let Some(observed) = modified_at else {
            return false;
        };
        indexed_size == size_bytes
            && system_time_to_millis(observed).ok() == system_time_to_millis(indexed).ok()
    }
}

impl DurableStateDb {
    #[allow(clippy::too_many_arguments)]
    pub fn set_sync_index(
        &mut self,
        path: &Path,
        content_hash: &str,
        size_bytes: u64,
        local_modified_at: Option<SystemTime>,
        remote_modified_at: Option<SystemTime>,
        last_op_id: &str,
        now: SystemTime,
    ) -> Result<(), StateDbError> {
        let local_modified_at_ms = local_modified_at.map(system_time_to_millis).transpose()?;
        let remote_modified_at_ms = remote_modified_at.map(system_time_to_millis).transpose()?;
        let path_text = path_to_text(path)?;
        self.one_spelling.get_mut().clear();
        self.connection.execute(
            "INSERT INTO sync_index
                 (path_text, fold_key, content_hash, size_bytes, local_modified_at_ms,
                  remote_modified_at_ms, last_op_id, updated_at_ms)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(path_text) DO UPDATE SET
                 content_hash = excluded.content_hash,
                 size_bytes = excluded.size_bytes,
                 local_modified_at_ms = excluded.local_modified_at_ms,
                 remote_modified_at_ms = excluded.remote_modified_at_ms,
                 last_op_id = excluded.last_op_id,
                 updated_at_ms = excluded.updated_at_ms,
                 hidden_since_ms = NULL",
            params![
                path_text,
                vapor_shared::paths::fold_name(&path_text),
                content_hash,
                i64::try_from(size_bytes).unwrap_or(i64::MAX),
                local_modified_at_ms,
                remote_modified_at_ms,
                last_op_id,
                system_time_to_millis(now)?
            ],
        )?;
        Ok(())
    }

    /// Records what the cloud object of `path` looks like now, for a row
    /// whose content did not change: the object was touched, or written
    /// again with the same bytes. The content, the local side, and the
    /// time of the last sync stay as they are.
    pub fn set_sync_index_remote_state(
        &mut self,
        path: &Path,
        remote_modified_at: SystemTime,
        last_op_id: &str,
    ) -> Result<(), StateDbError> {
        self.connection.execute(
            "UPDATE sync_index SET remote_modified_at_ms = ?, last_op_id = ? WHERE path_text = ?",
            params![
                system_time_to_millis(remote_modified_at)?,
                last_op_id,
                path_to_text(path)?
            ],
        )?;
        Ok(())
    }

    pub fn sync_index(&self, path: &Path) -> Result<Option<SyncIndexEntry>, StateDbError> {
        let path_text = path_to_text(path)?;
        let row = self
            .connection
            .query_row(
                "SELECT content_hash, size_bytes, local_modified_at_ms, remote_modified_at_ms,
                        last_op_id, updated_at_ms
                 FROM sync_index WHERE path_text = ?",
                params![path_text],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, Option<i64>>(2)?,
                        row.get::<_, Option<i64>>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, i64>(5)?,
                    ))
                },
            )
            .optional()?;
        row.map(
            |(
                content_hash,
                size_bytes,
                local_modified_at_ms,
                remote_modified_at_ms,
                last_op_id,
                updated_at_ms,
            )| {
                Ok(SyncIndexEntry {
                    path: path.to_path_buf(),
                    content_hash,
                    size_bytes: u64::try_from(size_bytes).unwrap_or(0),
                    local_modified_at: local_modified_at_ms
                        .map(millis_to_system_time)
                        .transpose()?,
                    remote_modified_at: remote_modified_at_ms
                        .map(millis_to_system_time)
                        .transpose()?,
                    last_op_id,
                    updated_at: millis_to_system_time(updated_at_ms)?,
                })
            },
        )
        .transpose()
    }

    /// Every synced path whose last transfer had this content, for move
    /// detection (a file that vanished at one path and appeared at
    /// another with the same bytes).
    pub fn sync_index_by_content(
        &self,
        content_hash: &str,
        size_bytes: u64,
    ) -> Result<Vec<SyncIndexEntry>, StateDbError> {
        let mut statement = self.connection.prepare(
            "SELECT path_text FROM sync_index WHERE content_hash = ? AND size_bytes = ? LIMIT 16",
        )?;
        let paths: Vec<String> = statement
            .query_map(
                params![content_hash, i64::try_from(size_bytes).unwrap_or(i64::MAX)],
                |row| row.get::<_, String>(0),
            )?
            .collect::<Result<_, _>>()?;
        let mut entries = Vec::new();
        for path in paths {
            if let Some(entry) = self.sync_index(&path_from_text(path))? {
                entries.push(entry);
            }
        }
        Ok(entries)
    }

    /// Every synced path whose remote object had this size and mtime
    /// at the last transfer: the cheap first filter for a cloud-side
    /// move (the object keeps both when it is renamed).
    pub fn sync_index_by_remote_state(
        &self,
        size_bytes: u64,
        remote_modified_at: SystemTime,
    ) -> Result<Vec<SyncIndexEntry>, StateDbError> {
        let mut statement = self.connection.prepare(
            "SELECT path_text FROM sync_index
             WHERE size_bytes = ? AND remote_modified_at_ms = ? LIMIT 16",
        )?;
        let paths: Vec<String> = statement
            .query_map(
                params![
                    i64::try_from(size_bytes).unwrap_or(i64::MAX),
                    system_time_to_millis(remote_modified_at)?
                ],
                |row| row.get::<_, String>(0),
            )?
            .collect::<Result<_, _>>()?;
        let mut entries = Vec::new();
        for path in paths {
            if let Some(entry) = self.sync_index(&path_from_text(path))? {
                entries.push(entry);
            }
        }
        Ok(entries)
    }

    /// Moves the row of `from` to `to`: the same file under a new
    /// spelling of its name. A row already at `to` is replaced.
    pub fn rekey_sync_index(&mut self, from: &Path, to: &Path) -> Result<(), StateDbError> {
        let (from, to) = (path_to_text(from)?, path_to_text(to)?);
        self.one_spelling.get_mut().clear();
        let transaction = Atomic::begin(&mut self.connection, self.batch_open)?;
        let moved = transaction
            .query_row(
                "SELECT 1 FROM sync_index WHERE path_text = ?",
                params![from],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if moved {
            transaction.execute("DELETE FROM sync_index WHERE path_text = ?", params![to])?;
            transaction.execute(
                "UPDATE sync_index SET path_text = ?, fold_key = ? WHERE path_text = ?",
                params![to, vapor_shared::paths::fold_name(&to), from],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    /// Moves every row under the folder `from` to the same place under
    /// `to`: the same folder under a new spelling of its name.
    pub fn rekey_sync_index_under(&mut self, from: &Path, to: &Path) -> Result<(), StateDbError> {
        let (lower, upper) = rows_under_bounds(from)?;
        self.one_spelling.get_mut().clear();
        let to = format!("{}{}", path_to_text(to)?, std::path::MAIN_SEPARATOR);
        let transaction = Atomic::begin(&mut self.connection, self.batch_open)?;
        transaction.execute(
            "DELETE FROM sync_index WHERE path_text IN (
                 SELECT ?1 || substr(path_text, ?2) FROM sync_index
                 WHERE path_text >= ?3 AND path_text < ?4)",
            params![to, lower.chars().count() as i64 + 1, lower, upper],
        )?;
        // A path separator composes with nothing, so the folded key of
        // a path is the folded prefix followed by the folded rest.
        let folded_to = vapor_shared::paths::fold_name(&to);
        let folded_from_chars = vapor_shared::paths::fold_name(&lower).chars().count() as i64;
        transaction.execute(
            "UPDATE sync_index SET path_text = ?1 || substr(path_text, ?2),
                                   fold_key = ?5 || substr(fold_key, ?6)
             WHERE path_text >= ?3 AND path_text < ?4",
            params![
                to,
                lower.chars().count() as i64 + 1,
                lower,
                upper,
                folded_to,
                folded_from_chars + 1
            ],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// A synced path that is another spelling of `path`, or lies under
    /// another spelling of it: the names fold together and the text
    /// differs. `None` when every row at and under `path` is spelled as
    /// `path` is, and when there is none.
    pub fn sync_index_row_spelled_otherwise(
        &self,
        path: &Path,
    ) -> Result<Option<PathBuf>, StateDbError> {
        let path_text = path_to_text(path)?;
        let folded = vapor_shared::paths::fold_name(&path_text);
        let twin: Option<String> = self
            .connection
            .prepare_cached(
                "SELECT path_text FROM sync_index WHERE fold_key = ?1 AND path_text <> ?2 LIMIT 1",
            )?
            .query_row(params![folded, path_text], |row| row.get(0))
            .optional()?;
        if let Some(twin) = twin {
            return Ok(Some(path_from_text(twin)));
        }
        if self.one_spelling.borrow().contains(path) {
            return Ok(None);
        }
        let (lower, upper) = rows_under_bounds(path)?;
        let (folded_lower, folded_upper) = rows_under_bounds(Path::new(&folded))?;
        // With no other spelling below, this reads every index entry
        // under the folder: no index answers "in this range of folded
        // keys and outside that range of paths" with a seek.
        let below: Option<String> = self
            .connection
            .prepare_cached(
                "SELECT path_text FROM sync_index
                 WHERE fold_key >= ?1 AND fold_key < ?2
                   AND NOT (path_text >= ?3 AND path_text < ?4)
                 LIMIT 1",
            )?
            .query_row(params![folded_lower, folded_upper, lower, upper], |row| {
                row.get(0)
            })
            .optional()?;
        if below.is_none() {
            let mut known = self.one_spelling.borrow_mut();
            if known.len() >= ONE_SPELLING_MEMO_LIMIT {
                known.clear();
            }
            known.insert(path.to_path_buf());
        }
        Ok(below.map(path_from_text))
    }

    /// Marks the row of `path`, and every row under it, as hidden by an
    /// ignore rule since `at`. While a rule hides a path nothing watches
    /// it, so the row no longer says what this device did to the file:
    /// a file that is gone when the rule goes was removed in that time,
    /// and that is not a deletion to carry to the cloud. The mark goes
    /// when a transfer writes the row again, or when a walk finds the
    /// file here with no rule on it.
    pub fn mark_sync_index_hidden(
        &mut self,
        path: &Path,
        at: SystemTime,
    ) -> Result<usize, StateDbError> {
        let (lower, upper) = rows_under_bounds(path)?;
        Ok(self.connection.execute(
            "UPDATE sync_index SET hidden_since_ms = ?1
             WHERE hidden_since_ms IS NULL
               AND (path_text = ?2 OR (path_text >= ?3 AND path_text < ?4))",
            params![
                system_time_to_millis(at)?,
                path_to_text(path)?,
                lower,
                upper
            ],
        )?)
    }

    /// Whether the row of `path` carries the mark of
    /// [`Self::mark_sync_index_hidden`].
    pub fn sync_index_was_hidden(&self, path: &Path) -> Result<bool, StateDbError> {
        Ok(self
            .connection
            .prepare_cached(
                "SELECT 1 FROM sync_index WHERE path_text = ? AND hidden_since_ms IS NOT NULL",
            )?
            .query_row(params![path_to_text(path)?], |_| Ok(()))
            .optional()?
            .is_some())
    }

    /// The files directly in `directory` whose rows carry that mark.
    pub fn sync_index_hidden_files_in(
        &self,
        directory: &Path,
    ) -> Result<Vec<String>, StateDbError> {
        let (lower, upper) = rows_under_bounds(directory)?;
        let mut statement = self.connection.prepare_cached(
            "SELECT substr(path_text, ?3) FROM sync_index
             WHERE hidden_since_ms IS NOT NULL
               AND path_text >= ?1 AND path_text < ?2
               AND instr(substr(path_text, ?3), ?4) = 0",
        )?;
        let names = statement
            .query_map(
                params![
                    lower,
                    upper,
                    i64::try_from(lower.chars().count() + 1).unwrap_or(i64::MAX),
                    std::path::MAIN_SEPARATOR.to_string()
                ],
                |row| row.get::<_, String>(0),
            )?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(names)
    }

    /// Takes that mark off the row of `path`.
    pub fn clear_sync_index_hidden(&mut self, path: &Path) -> Result<usize, StateDbError> {
        Ok(self.connection.execute(
            "UPDATE sync_index SET hidden_since_ms = NULL
             WHERE path_text = ? AND hidden_since_ms IS NOT NULL",
            params![path_to_text(path)?],
        )?)
    }

    /// Removes the row of `path` and every row under it.
    pub fn remove_sync_index_tree(&mut self, path: &Path) -> Result<usize, StateDbError> {
        let (lower, upper) = rows_under_bounds(path)?;
        Ok(self.connection.execute(
            "DELETE FROM sync_index
             WHERE path_text = ?1 OR (path_text >= ?2 AND path_text < ?3)",
            params![path_to_text(path)?, lower, upper],
        )?)
    }

    /// Whether any synced file lies under the folder `directory`.
    pub fn sync_index_has_rows_under(&self, directory: &Path) -> Result<bool, StateDbError> {
        let (lower, upper) = rows_under_bounds(directory)?;
        Ok(self
            .connection
            .query_row(
                "SELECT 1 FROM sync_index WHERE path_text >= ? AND path_text < ? LIMIT 1",
                params![lower, upper],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    /// The row of every synced file under the folder `directory`.
    pub fn sync_index_rows_under(
        &self,
        directory: &Path,
    ) -> Result<Vec<SyncIndexEntry>, StateDbError> {
        let (lower, upper) = rows_under_bounds(directory)?;
        let mut statement = self.connection.prepare_cached(
            "SELECT path_text, content_hash, size_bytes, local_modified_at_ms,
                    remote_modified_at_ms, last_op_id, updated_at_ms
             FROM sync_index WHERE path_text >= ? AND path_text < ?",
        )?;
        let rows = statement.query_map(params![lower, upper], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, Option<i64>>(3)?,
                row.get::<_, Option<i64>>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, i64>(6)?,
            ))
        })?;
        let mut entries = Vec::new();
        for row in rows {
            let (
                path_text,
                content_hash,
                size_bytes,
                local_modified_at_ms,
                remote_modified_at_ms,
                last_op_id,
                updated_at_ms,
            ) = row?;
            entries.push(SyncIndexEntry {
                path: path_from_text(path_text),
                content_hash,
                size_bytes: u64::try_from(size_bytes).unwrap_or(0),
                local_modified_at: local_modified_at_ms
                    .map(millis_to_system_time)
                    .transpose()?,
                remote_modified_at: remote_modified_at_ms
                    .map(millis_to_system_time)
                    .transpose()?,
                last_op_id,
                updated_at: millis_to_system_time(updated_at_ms)?,
            });
        }
        Ok(entries)
    }

    /// The names directly inside `directory` that the sync index holds
    /// a row for, or rows under. One index seek for each name, however
    /// many rows lie under it.
    pub fn sync_index_names_in(
        &self,
        directory: &Path,
    ) -> Result<std::collections::BTreeSet<String>, StateDbError> {
        let (lower, upper) = rows_under_bounds(directory)?;
        let mut from_row = self.connection.prepare_cached(
            "SELECT path_text FROM sync_index
             WHERE path_text >= ? AND path_text < ? ORDER BY path_text LIMIT 1",
        )?;
        let mut after_row = self.connection.prepare_cached(
            "SELECT path_text FROM sync_index
             WHERE path_text > ? AND path_text < ? ORDER BY path_text LIMIT 1",
        )?;
        let mut names = std::collections::BTreeSet::new();
        // The next seek starts at `position`, or just past it.
        let (mut position, mut inclusive) = (lower.clone(), true);
        loop {
            let statement = if inclusive {
                &mut from_row
            } else {
                &mut after_row
            };
            let found: Option<String> = statement
                .query_row(params![position, upper], |row| row.get(0))
                .optional()?;
            let Some(path) = found else {
                break;
            };
            let below = &path[lower.len()..];
            match below.split_once(std::path::MAIN_SEPARATOR) {
                Some((name, _)) => {
                    // Past every row under this name. A name that only
                    // starts the same way (`photos.txt` next to
                    // `photos/`) sorts before the folder's rows and was
                    // seen already.
                    position = rows_under_bounds(&directory.join(name))?.1;
                    inclusive = true;
                    names.insert(name.to_string());
                }
                None => {
                    names.insert(below.to_string());
                    position = path;
                    inclusive = false;
                }
            }
        }
        Ok(names)
    }

    /// Removes the row of `path` and every row under it, where the row
    /// was last written before `before`. Returns the rows removed.
    pub fn remove_sync_index_tree_written_before(
        &mut self,
        path: &Path,
        before: SystemTime,
    ) -> Result<usize, StateDbError> {
        let (lower, upper) = rows_under_bounds(path)?;
        Ok(self.connection.execute(
            "DELETE FROM sync_index
             WHERE (path_text = ?1 OR (path_text >= ?2 AND path_text < ?3))
               AND updated_at_ms < ?4",
            params![
                path_to_text(path)?,
                lower,
                upper,
                system_time_to_millis(before)?
            ],
        )?)
    }

    pub fn remove_sync_index(&mut self, path: &Path) -> Result<(), StateDbError> {
        self.connection.execute(
            "DELETE FROM sync_index WHERE path_text = ?",
            params![path_to_text(path)?],
        )?;
        Ok(())
    }

    /// Prunes terminally-failed intent records: those older than the
    /// retention window, plus any beyond the newest-N cap. Keeps the
    /// durable DB bounded after a large failure burst (e.g. a revoked
    /// OAuth token finalizing an entire backlog). Returns rows removed.
    pub fn prune_failed_intents(&mut self, now: SystemTime) -> Result<usize, StateDbError> {
        let now_ms = system_time_to_millis(now)?;
        let cutoff = now_ms.saturating_sub(
            i64::try_from(constants::state::FAILED_INTENT_RETENTION_MILLIS).unwrap_or(i64::MAX),
        );
        let mut pruned = self.connection.execute(
            "DELETE FROM failed_intents WHERE failed_at_ms < ?",
            params![cutoff],
        )?;
        // Bounded row cap: keep only the newest N by (failed_at_ms, id).
        let cap = i64::try_from(constants::state::MAX_FAILED_INTENTS_RETAINED).unwrap_or(i64::MAX);
        pruned += self.connection.execute(
            "DELETE FROM failed_intents
             WHERE id NOT IN (
                 SELECT id FROM failed_intents
                 ORDER BY failed_at_ms DESC, id DESC
                 LIMIT ?
             )",
            params![cap],
        )?;
        Ok(pruned)
    }
}

/// Rows that are blocked, as SQL: pending, failed at least
/// `BLOCKED_AFTER_ATTEMPTS` times, with a reason, and not an upload
/// that waits for room in the cloud (the status says that on its own).
fn blocked_predicate() -> String {
    format!(
        "state = '{STATE_PENDING}' AND attempt_count >= {} AND last_error IS NOT NULL
         AND substr(last_error, 1, {}) <> '{}'",
        constants::state::BLOCKED_AFTER_ATTEMPTS,
        constants::engine::CLOUD_FULL_WAIT_PREFIX.len(),
        constants::engine::CLOUD_FULL_WAIT_PREFIX.replace('\'', "''")
    )
}

/// Rows that wait in a retry backoff, as SQL: pending, not due, failed
/// before, and not an upload that waits for room in the cloud (that
/// wait has its own end).
fn waiting_retry_predicate() -> String {
    format!(
        "state = '{STATE_PENDING}' AND attempt_count > 0 AND available_at_ms > ?1
         AND (last_error IS NULL OR substr(last_error, 1, {}) <> '{}')",
        constants::engine::CLOUD_FULL_WAIT_PREFIX.len(),
        constants::engine::CLOUD_FULL_WAIT_PREFIX.replace('\'', "''")
    )
}

/// A new event for `path_text` at `observed_at`: the retries of that
/// path are due now. What stopped them (a file that could not be read,
/// a target that was locked) may be gone, and the event is the only
/// sign of that the daemon gets. A row whose last try is younger than
/// the grace is left alone: the event is most likely the trace of that
/// try, and waking on it would retry in a loop with no backoff.
fn wake_retries_of(
    connection: &Connection,
    path_text: &str,
    observed_at: SystemTime,
) -> Result<usize, StateDbError> {
    let now_ms = system_time_to_millis(observed_at)?;
    let grace_ms = i64::try_from(constants::state::RETRY_WAKE_GRACE_MILLIS).unwrap_or(i64::MAX);
    Ok(connection.execute(
        &format!(
            "UPDATE queue_intents SET available_at_ms = ?1
             WHERE path_text = ?2 AND {}
               AND last_attempt_at_ms IS NOT NULL AND last_attempt_at_ms <= ?3",
            waiting_retry_predicate()
        ),
        params![now_ms, path_text, now_ms.saturating_sub(grace_ms)],
    )?)
}

fn insert_intent(
    connection: &Connection,
    path: &Path,
    kind: PendingIntentKind,
    enqueued_at: SystemTime,
    available_at: SystemTime,
    source: crate::safeguards::IntentSource,
) -> Result<i64, StateDbError> {
    let enqueued_at_ms = system_time_to_millis(enqueued_at)?;
    let available_at_ms = system_time_to_millis(available_at)?;
    let priority_rank = crate::safeguards::durable_intent_priority_rank(path, kind, source);
    connection.execute(
        "INSERT INTO queue_intents (
            path_text,
            kind,
            state,
            priority_rank,
            enqueued_at_ms,
            available_at_ms,
            leased_at_ms,
            attempt_count,
            last_error
        ) VALUES (?, ?, ?, ?, ?, ?, NULL, 0, NULL)",
        params![
            path_to_text(path)?,
            intent_kind_label(kind),
            STATE_PENDING,
            i64::from(priority_rank),
            enqueued_at_ms,
            available_at_ms
        ],
    )?;
    Ok(connection.last_insert_rowid())
}

/// One answer the user can give to a decision.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DecisionOption {
    pub key: String,
    pub label: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DecisionScope {
    /// One path.
    Path,
    /// A batch of intents (a deletion burst).
    Batch,
    /// The whole profile is waiting.
    Profile,
}

impl DecisionScope {
    pub fn label(self) -> &'static str {
        match self {
            DecisionScope::Path => constants::decision::SCOPE_PATH,
            DecisionScope::Batch => constants::decision::SCOPE_BATCH,
            DecisionScope::Profile => constants::decision::SCOPE_PROFILE,
        }
    }

    fn parse(text: &str) -> Result<Self, StateDbError> {
        match text {
            constants::decision::SCOPE_PATH => Ok(Self::Path),
            constants::decision::SCOPE_BATCH => Ok(Self::Batch),
            constants::decision::SCOPE_PROFILE => Ok(Self::Profile),
            other => Err(StateDbError::InvalidStateValue(format!(
                "decision scope {other:?}"
            ))),
        }
    }
}

/// A durable question: an irreversible action whose evidence is
/// ambiguous, parked until the user answers. Lives in the profile's
/// state DB; `vapor decisions` reads and resolves it with or without a
/// running daemon, and the daemon applies the answer on its next tick.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecisionRecord {
    pub id: i64,
    pub kind: String,
    pub scope: DecisionScope,
    pub path: Option<PathBuf>,
    pub question: String,
    pub options: Vec<DecisionOption>,
    pub evidence: serde_json::Value,
    pub created_at: SystemTime,
    pub resolved_at: Option<SystemTime>,
    pub choice: Option<String>,
    pub applied_at: Option<SystemTime>,
    /// Intents parked behind this decision.
    pub held_intents: usize,
}

impl DecisionRecord {
    pub fn is_open(&self) -> bool {
        self.resolved_at.is_none()
    }
}

impl DurableStateDb {
    /// Opens a decision. Returns its id.
    #[allow(clippy::too_many_arguments)]
    pub fn create_decision(
        &mut self,
        kind: &str,
        scope: DecisionScope,
        path: Option<&Path>,
        question: &str,
        options: &[DecisionOption],
        evidence: &serde_json::Value,
        now: SystemTime,
    ) -> Result<i64, StateDbError> {
        let path_text = path.map(path_to_text).transpose()?;
        let options_json = serde_json::to_string(options)
            .map_err(|error| StateDbError::InvalidStateValue(error.to_string()))?;
        let evidence_json = serde_json::to_string(evidence)
            .map_err(|error| StateDbError::InvalidStateValue(error.to_string()))?;
        self.connection.execute(
            "INSERT INTO pending_decisions
                 (kind, scope, path_text, question, options_json, evidence_json, created_at_ms)
             VALUES (?, ?, ?, ?, ?, ?, ?)",
            params![
                kind,
                scope.label(),
                path_text,
                question,
                options_json,
                evidence_json,
                system_time_to_millis(now)?
            ],
        )?;
        Ok(self.connection.last_insert_rowid())
    }

    /// The unresolved decision of `kind` for `path` (or for the profile
    /// when `path` is `None`), if one is already open.
    pub fn open_decision(
        &self,
        kind: &str,
        path: Option<&Path>,
    ) -> Result<Option<DecisionRecord>, StateDbError> {
        let path_text = path.map(path_to_text).transpose()?;
        let id = self
            .connection
            .query_row(
                "SELECT id FROM pending_decisions
                 WHERE kind = ? AND resolved_at_ms IS NULL
                   AND ((path_text IS NULL AND ? IS NULL) OR path_text = ?)
                 ORDER BY id LIMIT 1",
                params![kind, path_text, path_text],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        match id {
            Some(id) => self.decision(id),
            None => Ok(None),
        }
    }

    /// The id of the open decision of `kind` that has no path of its
    /// own (a batch or a profile question), when there is one.
    pub fn open_decision_id(&self, kind: &str) -> Result<Option<i64>, StateDbError> {
        Ok(self
            .connection
            .prepare_cached(
                "SELECT id FROM pending_decisions
                 WHERE kind = ? AND resolved_at_ms IS NULL AND path_text IS NULL
                 ORDER BY id LIMIT 1",
            )?
            .query_row(params![kind], |row| row.get::<_, i64>(0))
            .optional()?)
    }

    pub fn decision(&self, id: i64) -> Result<Option<DecisionRecord>, StateDbError> {
        let mut records = self.decisions_where("id = ?", params![id])?;
        Ok(records.pop())
    }

    /// Every decision, open first, newest first within each group.
    pub fn decisions(&self, include_closed: bool) -> Result<Vec<DecisionRecord>, StateDbError> {
        if include_closed {
            self.decisions_where("1 = 1", [])
        } else {
            self.decisions_where("resolved_at_ms IS NULL", [])
        }
    }

    pub fn open_decision_count(&self) -> Result<usize, StateDbError> {
        let count = self.connection.query_row(
            "SELECT COUNT(*) FROM pending_decisions WHERE resolved_at_ms IS NULL",
            [],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(count as usize)
    }

    /// Keep-both conflict copies the user has not resolved: every path
    /// the sync index knows whose file name parses as a conflict copy,
    /// plus the copies still queued for their first upload (a copy
    /// enters the index only once it has synced, and the user should
    /// hear about it before that). Two partial indexes over the marker
    /// keep this a read of the matching rows.
    pub fn conflict_copy_paths(&self) -> Result<Vec<PathBuf>, StateDbError> {
        let mut statement = self
            .connection
            .prepare_cached(UNRESOLVED_CONFLICT_COPY_QUERY)?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        let mut paths = Vec::new();
        for row in rows {
            let path = path_from_text(row?);
            let is_conflict_copy = path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| crate::conflict::parse_conflict_copy_name(name).is_some());
            if is_conflict_copy {
                paths.push(path);
            }
        }
        Ok(paths)
    }

    /// Decisions the user answered that the daemon has not acted on.
    pub fn resolved_unapplied_decisions(&self) -> Result<Vec<DecisionRecord>, StateDbError> {
        self.decisions_where("resolved_at_ms IS NOT NULL AND applied_at_ms IS NULL", [])
    }

    fn decisions_where(
        &self,
        clause: &str,
        parameters: impl rusqlite::Params,
    ) -> Result<Vec<DecisionRecord>, StateDbError> {
        let mut statement = self.connection.prepare(&format!(
            "SELECT d.id, d.kind, d.scope, d.path_text, d.question, d.options_json,
                    d.evidence_json, d.created_at_ms, d.resolved_at_ms, d.choice, d.applied_at_ms,
                    (SELECT COUNT(*) FROM queue_intents q WHERE q.decision_id = d.id AND q.state = 'held')
             FROM pending_decisions d
             WHERE {clause}
             ORDER BY (d.resolved_at_ms IS NOT NULL) ASC, d.id DESC"
        ))?;
        let rows = statement.query_map(parameters, |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, i64>(7)?,
                row.get::<_, Option<i64>>(8)?,
                row.get::<_, Option<String>>(9)?,
                row.get::<_, Option<i64>>(10)?,
                row.get::<_, i64>(11)?,
            ))
        })?;
        let mut records = Vec::new();
        for row in rows {
            let (
                id,
                kind,
                scope,
                path_text,
                question,
                options_json,
                evidence_json,
                created_at_ms,
                resolved_at_ms,
                choice,
                applied_at_ms,
                held,
            ) = row?;
            records.push(DecisionRecord {
                id,
                kind,
                scope: DecisionScope::parse(&scope)?,
                path: path_text.map(path_from_text),
                question,
                options: serde_json::from_str(&options_json)
                    .map_err(|error| StateDbError::InvalidStateValue(error.to_string()))?,
                evidence: serde_json::from_str(&evidence_json).unwrap_or(serde_json::Value::Null),
                created_at: millis_to_system_time(created_at_ms)?,
                resolved_at: resolved_at_ms.map(millis_to_system_time).transpose()?,
                choice,
                applied_at: applied_at_ms.map(millis_to_system_time).transpose()?,
                held_intents: usize::try_from(held).unwrap_or(0),
            });
        }
        Ok(records)
    }

    /// Records the user's answer. Refuses an unknown option or an
    /// already-resolved decision.
    pub fn resolve_decision(
        &mut self,
        id: i64,
        choice: &str,
        now: SystemTime,
    ) -> Result<DecisionRecord, StateDbError> {
        let Some(record) = self.decision(id)? else {
            return Err(StateDbError::InvalidStateValue(format!(
                "decision {id} does not exist"
            )));
        };
        if !record.is_open() {
            return Err(StateDbError::InvalidStateValue(format!(
                "decision {id} was already resolved as {:?}",
                record.choice.as_deref().unwrap_or("")
            )));
        }
        if !record.options.iter().any(|option| option.key == choice) {
            return Err(StateDbError::InvalidStateValue(format!(
                "decision {id} has no option {choice:?}; choose one of {}",
                record
                    .options
                    .iter()
                    .map(|option| option.key.clone())
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
        // Conditional on the question still being open, so two answers
        // racing (two CLIs, or the CLI and a withdrawal) cannot both win.
        let claimed = self.connection.execute(
            "UPDATE pending_decisions SET resolved_at_ms = ?, choice = ?
             WHERE id = ? AND resolved_at_ms IS NULL",
            params![system_time_to_millis(now)?, choice, id],
        )?;
        if claimed == 0 {
            return Err(StateDbError::InvalidStateValue(format!(
                "decision {id} was answered or closed meanwhile"
            )));
        }
        self.decision(id)?
            .ok_or_else(|| StateDbError::InvalidStateValue(format!("decision {id} vanished")))
    }

    /// Puts an answered but unapplied decision back to open, so it can
    /// be asked again (an answer whose application failed half way).
    pub fn reopen_decision(&mut self, id: i64) -> Result<(), StateDbError> {
        self.connection.execute(
            "UPDATE pending_decisions SET resolved_at_ms = NULL, choice = NULL
             WHERE id = ? AND applied_at_ms IS NULL",
            params![id],
        )?;
        Ok(())
    }

    /// Whether a decision of `kind` for `path` was applied after `since`:
    /// the walk uses it to give an answer time to land before asking
    /// the same question again.
    pub fn decision_applied_since(
        &self,
        kind: &str,
        path: &Path,
        since: SystemTime,
    ) -> Result<bool, StateDbError> {
        let count = self.connection.query_row(
            "SELECT COUNT(*) FROM pending_decisions
             WHERE kind = ? AND path_text = ? AND applied_at_ms >= ?",
            params![kind, path_to_text(path)?, system_time_to_millis(since)?],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(count > 0)
    }

    /// Whether a decision of `kind` at `path` was ever answered with
    /// `choice`. An answer that means "stop asking" is honoured across
    /// walks and restarts through this.
    pub fn decision_answered(
        &self,
        kind: &str,
        path: &Path,
        choice: &str,
    ) -> Result<bool, StateDbError> {
        let count = self.connection.query_row(
            "SELECT COUNT(*) FROM pending_decisions
             WHERE kind = ? AND path_text = ? AND choice = ?",
            params![kind, path_to_text(path)?, choice],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(count > 0)
    }

    /// Closes an open decision the daemon no longer needs an answer to
    /// (the condition it asked about went away). Recorded as resolved
    /// and applied with the choice `withdrawn`, so the history says
    /// why it closed.
    pub fn withdraw_decision(&mut self, id: i64, now: SystemTime) -> Result<(), StateDbError> {
        let now_ms = system_time_to_millis(now)?;
        self.connection.execute(
            "UPDATE pending_decisions
             SET resolved_at_ms = ?, choice = 'withdrawn', applied_at_ms = ?
             WHERE id = ? AND resolved_at_ms IS NULL",
            params![now_ms, now_ms, id],
        )?;
        Ok(())
    }

    /// Rewrites an open decision's question, options, and evidence in
    /// place, so the id the user already saw stays valid while what it
    /// asks about changes (a conflict gaining or losing a version).
    pub fn update_open_decision(
        &mut self,
        id: i64,
        question: &str,
        options: &[DecisionOption],
        evidence: &serde_json::Value,
    ) -> Result<(), StateDbError> {
        let options_json = serde_json::to_string(options)
            .map_err(|error| StateDbError::InvalidStateValue(error.to_string()))?;
        let evidence_json = serde_json::to_string(evidence)
            .map_err(|error| StateDbError::InvalidStateValue(error.to_string()))?;
        self.connection.execute(
            "UPDATE pending_decisions SET question = ?, options_json = ?, evidence_json = ?
             WHERE id = ? AND resolved_at_ms IS NULL",
            params![question, options_json, evidence_json, id],
        )?;
        Ok(())
    }

    /// Adds a path to the decision's evidence (`paths` array, capped
    /// at `cap`; the count of held intents is tracked separately).
    pub fn append_decision_evidence_path(
        &mut self,
        id: i64,
        path: &Path,
        cap: usize,
    ) -> Result<(), StateDbError> {
        // Only the evidence: the full record counts the held intents,
        // and a burst appends once for each of them.
        let stored: Option<String> = self
            .connection
            .prepare_cached("SELECT evidence_json FROM pending_decisions WHERE id = ?")?
            .query_row(params![id], |row| row.get(0))
            .optional()?;
        let Some(mut evidence) =
            stored.and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        else {
            return Ok(());
        };
        let paths = evidence
            .as_object_mut()
            .and_then(|object| object.get_mut("paths"))
            .and_then(|paths| paths.as_array_mut());
        if let Some(paths) = paths
            && paths.len() < cap
        {
            paths.push(serde_json::Value::String(path.display().to_string()));
        } else {
            return Ok(());
        }
        let evidence_json = serde_json::to_string(&evidence)
            .map_err(|error| StateDbError::InvalidStateValue(error.to_string()))?;
        self.connection.execute(
            "UPDATE pending_decisions SET evidence_json = ? WHERE id = ?",
            params![evidence_json, id],
        )?;
        Ok(())
    }

    pub fn mark_decision_applied(&mut self, id: i64, now: SystemTime) -> Result<(), StateDbError> {
        self.connection.execute(
            "UPDATE pending_decisions SET applied_at_ms = ? WHERE id = ?",
            params![system_time_to_millis(now)?, id],
        )?;
        Ok(())
    }

    /// Parks a leased intent behind a decision. It is not leased again
    /// until `release_held` or removed by `drop_held`.
    pub fn hold_leased(&mut self, intent_id: i64, decision_id: i64) -> Result<bool, StateDbError> {
        let changed = self.connection.execute(
            "UPDATE queue_intents
             SET state = ?, leased_at_ms = NULL, decision_id = ?
             WHERE id = ? AND state = ?",
            params![STATE_HELD, decision_id, intent_id, STATE_LEASED],
        )?;
        Ok(changed == 1)
    }

    /// Parks every pending intent of `kind` behind `decision_id` and
    /// returns their paths. The mass-delete guard calls this when it
    /// trips, so the decision holds the whole burst the question
    /// describes, not only the deletions that reached the executor.
    pub fn hold_pending_of_kind(
        &mut self,
        kind: PendingIntentKind,
        decision_id: i64,
    ) -> Result<Vec<PathBuf>, StateDbError> {
        let mut statement = self.connection.prepare(
            "SELECT path_text FROM queue_intents WHERE kind = ? AND state = ? ORDER BY id ASC",
        )?;
        let rows = statement.query_map(params![intent_kind_label(kind), STATE_PENDING], |row| {
            row.get::<_, String>(0)
        })?;
        let mut paths = Vec::new();
        for row in rows {
            paths.push(path_from_text(row?));
        }
        drop(statement);
        self.connection.execute(
            "UPDATE queue_intents SET state = ?, decision_id = ? WHERE kind = ? AND state = ?",
            params![
                STATE_HELD,
                decision_id,
                intent_kind_label(kind),
                STATE_PENDING
            ],
        )?;
        Ok(paths)
    }

    /// Drops the held intent of `kind` at `path`, if any, and returns
    /// the decision it was held behind. The other side already applied
    /// the deletion, so there is nothing left to ask about for it.
    pub fn drop_held_at(
        &mut self,
        path: &Path,
        kind: PendingIntentKind,
    ) -> Result<Option<i64>, StateDbError> {
        let path_text = path_to_text(path)?;
        let decision_id: Option<i64> = self
            .connection
            .query_row(
                "SELECT decision_id FROM queue_intents
                 WHERE path_text = ? AND kind = ? AND state = ?",
                params![path_text, intent_kind_label(kind), STATE_HELD],
                |row| row.get(0),
            )
            .optional()?;
        if decision_id.is_some() {
            self.connection.execute(
                "DELETE FROM queue_intents WHERE path_text = ? AND kind = ? AND state = ?",
                params![path_text, intent_kind_label(kind), STATE_HELD],
            )?;
        }
        Ok(decision_id)
    }

    pub fn held_intents(&self, decision_id: i64) -> Result<Vec<DurableIntentRecord>, StateDbError> {
        let mut statement = self.connection.prepare(&format!(
            "SELECT {INTENT_COLUMNS} FROM queue_intents
             WHERE decision_id = ? AND state = ?
             ORDER BY id ASC"
        ))?;
        let rows = statement.query_map(params![decision_id, STATE_HELD], intent_from_row)?;
        let mut records = Vec::new();
        for row in rows {
            records.push(raw_intent_to_record(row?)?);
        }
        Ok(records)
    }

    pub fn held_intent_count(&self) -> Result<usize, StateDbError> {
        count_intents(&self.connection, Some(STATE_HELD))
    }

    /// Returns every intent held behind `decision_id` to the pending
    /// state, ready now and marked approved so the guard that held it
    /// lets it through.
    pub fn release_held(
        &mut self,
        decision_id: i64,
        now: SystemTime,
    ) -> Result<usize, StateDbError> {
        let changed = self.connection.execute(
            "UPDATE queue_intents
             SET state = ?, available_at_ms = ?, decision_id = NULL,
                 approved = MAX(approved, 1)
             WHERE decision_id = ? AND state = ?",
            params![
                STATE_PENDING,
                system_time_to_millis(now)?,
                decision_id,
                STATE_HELD
            ],
        )?;
        Ok(changed)
    }

    /// Number of paths the sync index knows: the size of the synced
    /// tree the deletion guard measures its ratio against.
    pub fn sync_index_count(&self) -> Result<usize, StateDbError> {
        let count = self
            .connection
            .query_row("SELECT COUNT(*) FROM sync_index", [], |row| {
                row.get::<_, i64>(0)
            })?;
        Ok(count as usize)
    }

    /// Whether the queue still holds intent `id` in a working state.
    /// An in-flight intent checks this before an irreversible step, so
    /// a row dropped meanwhile (a root recovery) is honoured.
    pub fn intent_is_queued(&self, id: i64) -> Result<bool, StateDbError> {
        let count = self.connection.query_row(
            "SELECT COUNT(*) FROM queue_intents WHERE id = ? AND state IN (?, ?)",
            params![id, STATE_PENDING, STATE_LEASED],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(count > 0)
    }

    /// Removes every queued (pending or leased) intent of `kind`. A
    /// root recovery uses it: deletions observed while a root was
    /// going away describe the root's absence, not the user's intent.
    pub fn discard_queued_of_kind(
        &mut self,
        kind: PendingIntentKind,
    ) -> Result<usize, StateDbError> {
        let changed = self.connection.execute(
            "DELETE FROM queue_intents WHERE kind = ? AND state IN (?, ?)",
            params![intent_kind_label(kind), STATE_PENDING, STATE_LEASED],
        )?;
        Ok(changed)
    }

    /// Removes every intent held behind `decision_id`.
    pub fn drop_held(&mut self, decision_id: i64) -> Result<usize, StateDbError> {
        let changed = self.connection.execute(
            "DELETE FROM queue_intents WHERE decision_id = ? AND state = ?",
            params![decision_id, STATE_HELD],
        )?;
        Ok(changed)
    }

    /// Enqueues a download whose remote object lives at `remote_path`
    /// rather than at the local path's mirror.
    pub fn enqueue_download_from(
        &mut self,
        local_path: &Path,
        remote_path: &str,
        now: SystemTime,
    ) -> Result<i64, StateDbError> {
        let path_text = path_to_text(local_path)?;
        let now_ms = system_time_to_millis(now)?;
        let rank = i64::from(crate::safeguards::durable_intent_priority_rank(
            local_path,
            PendingIntentKind::Download,
            crate::safeguards::IntentSource::Fresh,
        ));
        self.connection.execute(
            "INSERT INTO queue_intents
                 (path_text, kind, state, priority_rank, enqueued_at_ms, available_at_ms, remote_path_text)
             VALUES (?, ?, ?, ?, ?, ?, ?)",
            params![
                path_text,
                intent_kind_label(PendingIntentKind::Download),
                STATE_PENDING,
                rank,
                now_ms,
                now_ms,
                remote_path
            ],
        )?;
        Ok(self.connection.last_insert_rowid())
    }

    /// Records that `remote_path` is materialized locally at `local_path`
    /// holding the remote version `remote_hash`.
    pub fn record_name_alias(
        &mut self,
        remote_path: &str,
        local_path: &Path,
        remote_hash: &str,
        now: SystemTime,
    ) -> Result<(), StateDbError> {
        self.connection.execute(
            "INSERT INTO name_aliases (remote_path_text, local_path_text, remote_hash, created_at_ms)
             VALUES (?, ?, ?, ?)
             ON CONFLICT(remote_path_text) DO UPDATE SET
                 local_path_text = excluded.local_path_text,
                 remote_hash = excluded.remote_hash,
                 created_at_ms = excluded.created_at_ms",
            params![
                remote_path,
                path_to_text(local_path)?,
                remote_hash,
                system_time_to_millis(now)?
            ],
        )?;
        Ok(())
    }

    /// `(local path, remote hash)` the remote name is materialized as.
    pub fn name_alias(&self, remote_path: &str) -> Result<Option<(PathBuf, String)>, StateDbError> {
        let row = self
            .connection
            .query_row(
                "SELECT local_path_text, remote_hash FROM name_aliases WHERE remote_path_text = ?",
                params![remote_path],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        Ok(row.map(|(local, hash)| (path_from_text(local), hash)))
    }

    /// The remote path a local file is materialized from, when its cloud
    /// object lives under a name this filesystem cannot hold next to
    /// another local name.
    pub fn alias_remote_for_local(
        &self,
        local_path: &Path,
    ) -> Result<Option<String>, StateDbError> {
        let remote = self
            .connection
            .query_row(
                "SELECT remote_path_text FROM name_aliases WHERE local_path_text = ?",
                params![path_to_text(local_path)?],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        Ok(remote)
    }

    /// Every alias under `directory` (direct children only), as
    /// `(remote file name, local file name)`.
    pub fn name_aliases_in(&self, directory: &Path) -> Result<Vec<(String, String)>, StateDbError> {
        let mut prefix = path_to_text(directory)?;
        if !prefix.ends_with(std::path::MAIN_SEPARATOR) {
            prefix.push(std::path::MAIN_SEPARATOR);
        }
        let mut statement = self
            .connection
            .prepare("SELECT remote_path_text, local_path_text FROM name_aliases")?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut found = Vec::new();
        for row in rows {
            let (remote, local) = row?;
            let Some(rest) = local.strip_prefix(&prefix) else {
                continue;
            };
            if rest.contains(std::path::MAIN_SEPARATOR) {
                continue;
            }
            let remote_name = remote.rsplit('/').next().unwrap_or(&remote).to_string();
            found.push((remote_name, rest.to_string()));
        }
        Ok(found)
    }

    /// Moves the aliases of the files under the folder `from` to the
    /// folder `to`, on both sides: the local folder changed its
    /// spelling, and its cloud folder moved from `remote_from` to
    /// `remote_to` with it. An alias that kept the old spelling would
    /// not be found for its file any more, and the file would be
    /// compared with a cloud name that holds nothing.
    pub fn rekey_name_aliases_under(
        &mut self,
        from: &Path,
        to: &Path,
        remote_from: &str,
        remote_to: &str,
    ) -> Result<usize, StateDbError> {
        let (lower, upper) = rows_under_bounds(from)?;
        let to_prefix = format!("{}{}", path_to_text(to)?, std::path::MAIN_SEPARATOR);
        let (remote_from, remote_to) = (format!("{remote_from}/"), format!("{remote_to}/"));
        Ok(self.connection.execute(
            "UPDATE name_aliases
             SET local_path_text = ?1 || substr(local_path_text, ?2),
                 remote_path_text = ?3 || substr(remote_path_text, ?4)
             WHERE local_path_text >= ?5 AND local_path_text < ?6
               AND substr(remote_path_text, 1, ?7) = ?8",
            params![
                to_prefix,
                lower.chars().count() as i64 + 1,
                remote_to,
                remote_from.chars().count() as i64 + 1,
                lower,
                upper,
                remote_from.chars().count() as i64,
                remote_from,
            ],
        )?)
    }

    pub fn remove_name_alias_for_local(&mut self, local_path: &Path) -> Result<(), StateDbError> {
        self.connection.execute(
            "DELETE FROM name_aliases WHERE local_path_text = ?",
            params![path_to_text(local_path)?],
        )?;
        Ok(())
    }

    pub fn remove_name_alias(&mut self, remote_path: &str) -> Result<(), StateDbError> {
        self.connection.execute(
            "DELETE FROM name_aliases WHERE remote_path_text = ?",
            params![remote_path],
        )?;
        Ok(())
    }
}

/// Whether a SQLite error means the file is genuinely not a usable
/// database (structural corruption or "not a database"), as opposed to a
/// transient/environmental failure (disk full, I/O error, busy lock,
/// permission). Only the former justifies quarantining the durable state.
fn is_corruption_error(error: &rusqlite::Error) -> bool {
    matches!(
        error.sqlite_error_code(),
        Some(rusqlite::ErrorCode::DatabaseCorrupt) | Some(rusqlite::ErrorCode::NotADatabase)
    )
}

fn configure_connection(connection: &mut Connection) -> Result<(), StateDbError> {
    // `synchronous = NORMAL` is the documented durability point for WAL
    // mode: the database can never corrupt, and at most the final
    // transaction(s) before an OS crash / power loss are rolled back.
    // The queue's at-least-once semantics plus the startup whole-scope
    // reconcile already reconstruct anything lost that way, so paying
    // `FULL`'s per-commit fsync bought nothing the design needs.
    connection.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         PRAGMA foreign_keys = ON;",
    )?;
    connection.busy_handler(Some(wait_for_the_lock))?;
    Ok(())
}

/// How many times a connection asks again for a lock that another
/// connection holds, one millisecond apart: about five seconds.
const LOCK_TRIES: i32 = 5_000;

/// The wait for a lock that another connection holds. SQLite's own
/// wait grows to a tenth of a second between two tries. A daemon in a
/// burst holds the write lock for the length of each tick and frees it
/// for an instant between two ticks, so a command that asked ten times
/// a second waited up to three seconds for its turn (measured, with a
/// burst of 12 000 files). At one try each millisecond the longest
/// wait was a third of a second.
fn wait_for_the_lock(tries: i32) -> bool {
    if tries >= LOCK_TRIES {
        return false;
    }
    std::thread::sleep(Duration::from_millis(1));
    true
}

fn ensure_schema(connection: &mut Connection) -> Result<(), StateDbError> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;

    // Validate the recorded schema version BEFORE running any DDL: a
    // database of another layout must fail with the clean
    // `SchemaVersionMismatch` instead of whatever confusing SQLite
    // error a conflicting `CREATE TABLE / INDEX` would produce first.
    let schema_meta_exists = transaction
        .query_row(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'schema_meta'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .is_some();
    if schema_meta_exists
        && let Some(found) = read_schema_version(&transaction)?
        && found != CURRENT_SCHEMA_VERSION
    {
        return Err(StateDbError::SchemaVersionMismatch {
            found,
            expected: CURRENT_SCHEMA_VERSION,
        });
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
             state TEXT NOT NULL CHECK(state IN ('pending', 'leased', 'held')),
             priority_rank INTEGER NOT NULL DEFAULT 4 CHECK(priority_rank >= 0),
             enqueued_at_ms INTEGER NOT NULL,
             available_at_ms INTEGER NOT NULL,
             leased_at_ms INTEGER,
             attempt_count INTEGER NOT NULL DEFAULT 0 CHECK(attempt_count >= 0),
             last_error TEXT,
             decision_id INTEGER,
             remote_path_text TEXT,
             approved INTEGER NOT NULL DEFAULT 0,
             last_attempt_at_ms INTEGER
         );
         -- A durable question for the user: an irreversible action whose
         -- evidence is ambiguous. Held intents reference it; the CLI
         -- resolves it; the daemon applies the answer.
         CREATE TABLE IF NOT EXISTS pending_decisions (
             id INTEGER PRIMARY KEY AUTOINCREMENT,
             kind TEXT NOT NULL,
             scope TEXT NOT NULL CHECK(scope IN ('path', 'batch', 'profile')),
             path_text TEXT,
             question TEXT NOT NULL,
             options_json TEXT NOT NULL,
             evidence_json TEXT NOT NULL,
             created_at_ms INTEGER NOT NULL,
             resolved_at_ms INTEGER,
             choice TEXT,
             applied_at_ms INTEGER
         );
         CREATE INDEX IF NOT EXISTS idx_pending_decisions_open
             ON pending_decisions(applied_at_ms, kind, path_text);
         -- A remote name this filesystem cannot hold next to a
         -- differently-cased sibling, materialized under another local
         -- name (a keep-both copy). The hash records which remote
         -- version the copy holds.
         CREATE TABLE IF NOT EXISTS name_aliases (
             remote_path_text TEXT PRIMARY KEY,
             local_path_text TEXT NOT NULL,
             remote_hash TEXT NOT NULL,
             created_at_ms INTEGER NOT NULL
         );
         -- Lease order: priority class first, then readiness, then id.
         CREATE INDEX IF NOT EXISTS idx_queue_intents_ready
             ON queue_intents(state, priority_rank, available_at_ms, id);
         -- Supports the coalesced-enqueue dedup lookup (path_text + kind +
         -- state) so a large pending queue does not turn every ingest
         -- flush into a full table scan.
         CREATE INDEX IF NOT EXISTS idx_queue_intents_path
             ON queue_intents(path_text, kind, state);
         -- Supports the diagnostics list ordering (available_at_ms, id)
         -- without a full scan + temp b-tree sort on a deep queue.
         CREATE INDEX IF NOT EXISTS idx_queue_intents_order
             ON queue_intents(available_at_ms, id);
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
             fold_key TEXT NOT NULL,
             content_hash TEXT NOT NULL,
             size_bytes INTEGER NOT NULL CHECK(size_bytes >= 0),
             local_modified_at_ms INTEGER,
             remote_modified_at_ms INTEGER,
             last_op_id TEXT NOT NULL,
             updated_at_ms INTEGER NOT NULL,
             -- Set while an ignore rule hides the path: what happened
             -- to the file in that time was not watched.
             hidden_since_ms INTEGER
         );
         CREATE INDEX IF NOT EXISTS idx_sync_index_hidden
             ON sync_index(path_text) WHERE hidden_since_ms IS NOT NULL;
         CREATE INDEX IF NOT EXISTS idx_sync_index_content
             ON sync_index(content_hash, size_bytes);
         -- The path with every name folded: one seek finds the rows a
         -- path has under another spelling of its name.
         CREATE INDEX IF NOT EXISTS idx_sync_index_fold
             ON sync_index(fold_key, path_text);
         CREATE INDEX IF NOT EXISTS idx_sync_index_remote_state
             ON sync_index(size_bytes, remote_modified_at_ms);
         -- Partial indexes holding only the rows whose name carries the
         -- conflict-copy marker, so counting unresolved conflicts for
         -- every status publish reads a handful of rows instead of
         -- scanning the whole index and queue. The predicate must be
         -- spelled exactly as in `conflict_copy_paths` for
         -- the planner to pick the index.
         CREATE INDEX IF NOT EXISTS idx_sync_index_conflict_copy
             ON sync_index(path_text) WHERE path_text LIKE '%~conflict-%';
         CREATE INDEX IF NOT EXISTS idx_queue_intents_conflict_copy
             ON queue_intents(path_text, kind) WHERE path_text LIKE '%~conflict-%';",
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

enum QuarantineReason {
    Corrupt(String),
    OtherSchema(i64),
}

/// Removes the databases set aside next to `path` longer ago than the
/// retention window. The time is the one in the file name.
fn prune_quarantined(path: &Path, now: SystemTime) {
    let (Some(directory), Some(name)) = (path.parent(), path.file_name()) else {
        return;
    };
    let name = name.to_string_lossy();
    let Ok(now_ms) = system_time_to_millis(now) else {
        return;
    };
    let cutoff =
        now_ms.saturating_sub(i64::try_from(QUARANTINE_RETENTION.as_millis()).unwrap_or(i64::MAX));
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let file_name = file_name.to_string_lossy();
        let Some(suffix) = file_name.strip_prefix(&*name) else {
            continue;
        };
        if !(suffix.starts_with(".corrupt-") || suffix.starts_with(".schema-v")) {
            continue;
        }
        let set_aside_at = suffix
            .rsplit('-')
            .next()
            .and_then(|text| text.parse::<i64>().ok());
        if set_aside_at.is_some_and(|at| at < cutoff) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
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

/// Counts queue rows in `state`, or the workable rows (pending and
/// leased) when `state` is `None`.
fn count_intents(connection: &Connection, state: Option<&str>) -> Result<usize, StateDbError> {
    let count = match state {
        Some(state) => connection.query_row(
            "SELECT COUNT(*) FROM queue_intents WHERE state = ?",
            params![state],
            |row| row.get::<_, i64>(0),
        )?,
        None => connection.query_row(
            "SELECT COUNT(*) FROM queue_intents WHERE state IN (?, ?)",
            params![STATE_PENDING, STATE_LEASED],
            |row| row.get::<_, i64>(0),
        )?,
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
    i64,
    Option<i64>,
    i64,
    Option<String>,
    Option<String>,
    i64,
    String,
    Option<i64>,
);

const INTENT_COLUMNS: &str = "id, path_text, kind, priority_rank, enqueued_at_ms, available_at_ms, \
                              leased_at_ms, attempt_count, last_error, remote_path_text, approved, \
                              state, decision_id";

fn intent_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawIntentRow> {
    Ok((
        row.get::<_, i64>(0)?,
        row.get::<_, String>(1)?,
        row.get::<_, String>(2)?,
        row.get::<_, i64>(3)?,
        row.get::<_, i64>(4)?,
        row.get::<_, i64>(5)?,
        row.get::<_, Option<i64>>(6)?,
        row.get::<_, i64>(7)?,
        row.get::<_, Option<String>>(8)?,
        row.get::<_, Option<String>>(9)?,
        row.get::<_, i64>(10)?,
        row.get::<_, String>(11)?,
        row.get::<_, Option<i64>>(12)?,
    ))
}

fn raw_intent_to_record(raw: RawIntentRow) -> Result<DurableIntentRecord, StateDbError> {
    let (
        id,
        path_text,
        kind,
        priority_rank,
        enqueued_at_ms,
        available_at_ms,
        leased_at_ms,
        attempt_count,
        last_error,
        remote_path_text,
        approved,
        state,
        decision_id,
    ) = raw;
    Ok(DurableIntentRecord {
        id,
        path: path_from_text(path_text),
        kind: intent_kind_from_label(&kind)?,
        priority_rank: u8::try_from(priority_rank).unwrap_or(u8::MAX),
        enqueued_at: millis_to_system_time(enqueued_at_ms)?,
        available_at: millis_to_system_time(available_at_ms)?,
        leased_at: leased_at_ms.map(millis_to_system_time).transpose()?,
        attempt_count: validate_attempt_count(attempt_count)?,
        last_error: last_error.map(|value| sanitize_persisted_error(&value)),
        remote_path: remote_path_text,
        approved: approved != 0,
        names_path: approved == Approval::NamedPath.stored(),
        held_by: (state == STATE_HELD).then_some(decision_id).flatten(),
    })
}

fn fetch_intent(
    connection: &Connection,
    id: i64,
) -> Result<Option<DurableIntentRecord>, StateDbError> {
    let raw_intent = connection
        .query_row(
            &format!(
                "SELECT {INTENT_COLUMNS}
                 FROM queue_intents
                 WHERE id = ?"
            ),
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

/// The durable spelling of a path. On Windows a path may arrive with
/// either separator (`docs/keep.txt` joined onto `C:\\watch`), and the
/// same file must key the same row, so the text is rebuilt from the
/// path's components, which spells every separator the OS way.
fn path_to_text(path: &Path) -> Result<String, StateDbError> {
    #[cfg(windows)]
    let normalized: PathBuf = path.components().collect();
    #[cfg(windows)]
    let path = normalized.as_path();
    path.to_str()
        .map(|s| s.to_string())
        .ok_or_else(|| StateDbError::NonUtf8Path(path.display().to_string()))
}

/// The key range that holds every path under `directory`: from the
/// folder's path plus a separator, up to the same text with the
/// separator's successor. A range on the primary key, so no scan.
fn rows_under_bounds(directory: &Path) -> Result<(String, String), StateDbError> {
    let base = path_to_text(directory)?;
    let separator = std::path::MAIN_SEPARATOR;
    let after = char::from_u32(separator as u32 + 1).unwrap_or(separator);
    Ok((format!("{base}{separator}"), format!("{base}{after}")))
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
    fn only_the_uploads_that_wait_for_room_in_the_cloud_end_their_wait() {
        let temp_dir = TempDir::new().expect("temp dir");
        let mut database =
            DurableStateDb::open(temp_dir.path().join("state/vapor.sqlite")).expect("open");
        let waits_for_room = PathBuf::from("/tmp/vapor-root/big.bin");
        let rate_limited = PathBuf::from("/tmp/vapor-root/other.bin");
        let room_message = format!(
            "{}the volume is full (big.bin)",
            constants::engine::CLOUD_FULL_WAIT_PREFIX
        );
        for (path, message) in [
            (&waits_for_room, room_message.as_str()),
            (&rate_limited, "rate limited"),
        ] {
            database
                .enqueue_intent(path, PendingIntentKind::Upload, timestamp_ms(100))
                .expect("enqueue");
            let leased = database
                .lease_next_ready(timestamp_ms(100))
                .expect("lease")
                .expect("leased");
            database
                .requeue_leased(leased.id, timestamp_ms(900_000), Some(message))
                .expect("requeue");
        }
        assert_eq!(
            database.wait_for_cloud_room_message().expect("message"),
            Some("the volume is full (big.bin)".to_string())
        );

        assert_eq!(
            database
                .end_wait_for_cloud_room(timestamp_ms(200))
                .expect("end the wait"),
            1
        );

        let due = database
            .lease_next_ready(timestamp_ms(200))
            .expect("lease")
            .expect("the upload that waited for room is due");
        assert_eq!(due.path, waits_for_room);
        assert!(
            database
                .lease_next_ready(timestamp_ms(200))
                .expect("lease")
                .is_none(),
            "a row that waits for another reason keeps its wait"
        );
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

    /// Fails the one queued intent `attempts` times in a row, each try
    /// a minute after the one before. Returns the time of the last try.
    fn fail_repeatedly(database: &mut DurableStateDb, attempts: u64) -> u64 {
        let mut now_ms = 0;
        for attempt in 0..attempts {
            now_ms = (attempt + 1) * 3_600_000;
            let leased = database
                .lease_next_ready(timestamp_ms(now_ms))
                .expect("lease")
                .expect("the retry is due an hour later");
            database
                .schedule_retry(
                    leased.id,
                    RetryFailureKind::Transient,
                    "cannot stat local file: Permission denied",
                    timestamp_ms(now_ms),
                )
                .expect("schedule retry");
        }
        now_ms
    }

    #[test]
    fn an_intent_that_keeps_failing_is_blocked_and_not_queued_work() {
        let temp_dir = TempDir::new().expect("temp dir");
        let mut database =
            DurableStateDb::open(temp_dir.path().join("state/vapor.sqlite")).expect("open");
        let path = PathBuf::from("/tmp/vapor-root/locked.txt");
        database
            .enqueue_intent(&path, PendingIntentKind::Upload, timestamp_ms(0))
            .expect("enqueue");

        fail_repeatedly(
            &mut database,
            u64::from(constants::state::BLOCKED_AFTER_ATTEMPTS) - 1,
        );
        assert_eq!(database.blocked_depth().expect("blocked"), 0);

        // One more failed try: the file is blocked, and named.
        let leased = database
            .lease_next_ready(timestamp_ms(100 * 3_600_000))
            .expect("lease")
            .expect("due");
        database
            .schedule_retry(
                leased.id,
                RetryFailureKind::Transient,
                "cannot stat local file: Permission denied",
                timestamp_ms(100 * 3_600_000),
            )
            .expect("schedule retry");
        assert_eq!(database.blocked_depth().expect("blocked"), 1);
        let blocked = database.blocked_intents(10).expect("list");
        assert_eq!(blocked.len(), 1);
        assert_eq!(blocked[0].path, path);
        assert_eq!(
            blocked[0].last_error.as_deref(),
            Some("cannot stat local file: Permission denied")
        );
    }

    #[test]
    fn a_new_event_for_a_path_makes_its_retry_due_unless_the_try_just_failed() {
        let temp_dir = TempDir::new().expect("temp dir");
        let mut database =
            DurableStateDb::open(temp_dir.path().join("state/vapor.sqlite")).expect("open");
        let path = PathBuf::from("/tmp/vapor-root/locked.txt");
        database
            .enqueue_intent(&path, PendingIntentKind::Download, timestamp_ms(0))
            .expect("enqueue");
        let failed_at = fail_repeatedly(&mut database, 8);
        let event = |database: &mut DurableStateDb, at_ms: u64| {
            database
                .enqueue_intents_coalesced(
                    &[(path.clone(), PendingIntentKind::Upload, timestamp_ms(at_ms))],
                    crate::safeguards::IntentSource::Fresh,
                )
                .expect("event")
        };
        let download_is_due = |database: &DurableStateDb, at_ms: u64| {
            database
                .list_queue_intents(10)
                .expect("list")
                .into_iter()
                .any(|intent| {
                    intent.kind == PendingIntentKind::Download
                        && intent.available_at <= timestamp_ms(at_ms)
                })
        };

        // An event two seconds after the try is most likely its own
        // trace: the backoff stands.
        event(&mut database, failed_at + 2_000);
        assert!(!download_is_due(&database, failed_at + 2_000));

        // An event a minute later is the user: the lock is gone, maybe.
        event(&mut database, failed_at + 60_000);
        assert!(download_is_due(&database, failed_at + 60_000));
    }

    #[test]
    fn a_sync_on_request_makes_every_retry_due_but_not_an_upload_that_waits_for_room() {
        let temp_dir = TempDir::new().expect("temp dir");
        let mut database =
            DurableStateDb::open(temp_dir.path().join("state/vapor.sqlite")).expect("open");
        let retried = PathBuf::from("/tmp/vapor-root/retried.txt");
        let waiting = PathBuf::from("/tmp/vapor-root/waits-for-room.txt");
        database
            .enqueue_intent(&retried, PendingIntentKind::Upload, timestamp_ms(0))
            .expect("enqueue");
        let failed_at = fail_repeatedly(&mut database, 3);
        database
            .enqueue_intent(&waiting, PendingIntentKind::Upload, timestamp_ms(failed_at))
            .expect("enqueue");
        let leased = database
            .lease_next_ready(timestamp_ms(failed_at))
            .expect("lease")
            .expect("the new upload is due");
        assert_eq!(leased.path, waiting);
        database
            .requeue_leased(
                leased.id,
                timestamp_ms(failed_at + 900_000),
                Some(&format!(
                    "{}the cloud is full",
                    constants::engine::CLOUD_FULL_WAIT_PREFIX
                )),
            )
            .expect("wait for room");

        assert_eq!(
            database
                .wake_all_retries(timestamp_ms(failed_at + 1_000))
                .expect("wake"),
            1
        );
        let due = database
            .lease_next_ready(timestamp_ms(failed_at + 1_000))
            .expect("lease")
            .expect("the retry is due");
        assert_eq!(due.path, retried);
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

    #[cfg(unix)]
    #[test]
    fn path_to_text_returns_utf8_for_ascii_paths() {
        let path = PathBuf::from("/tmp/vapor/file.txt");
        let text = path_to_text(&path).expect("ascii path is UTF-8");
        assert_eq!(text, "/tmp/vapor/file.txt");
    }

    #[cfg(windows)]
    #[test]
    fn path_to_text_spells_every_separator_the_os_way() {
        let mixed = PathBuf::from("C:\\watch\\docs/keep.txt");
        assert_eq!(
            path_to_text(&mixed).expect("utf-8"),
            "C:\\watch\\docs\\keep.txt"
        );
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
    fn a_batch_is_one_transaction_that_ends_with_its_commit() {
        let temp = tempfile::TempDir::new().expect("temp");
        let path = temp.path().join("vapor.sqlite");
        let mut daemon = DurableStateDb::open(&path).expect("open");
        let reader = DurableStateDb::open(&path).expect("second connection");
        let file = |name: &str| PathBuf::from("/tmp/vapor-root").join(name);

        daemon.begin_batch().expect("begin");
        daemon
            .enqueue_intent(&file("a.txt"), PendingIntentKind::Upload, timestamp_ms(1))
            .expect("one statement");
        // A write of several statements is a part of the batch, not a
        // transaction of its own.
        daemon
            .enqueue_intents_coalesced(
                &[(file("b.txt"), PendingIntentKind::Upload, timestamp_ms(2))],
                crate::safeguards::IntentSource::Fresh,
            )
            .expect("several statements");
        daemon
            .set_sync_index(
                &file("photos/a.jpg"),
                "hash",
                1,
                None,
                None,
                "op",
                timestamp_ms(1),
            )
            .expect("seed");
        daemon
            .rekey_sync_index_under(&file("photos"), &file("Photos"))
            .expect("several statements");
        assert_eq!(daemon.pending_depth().expect("own view"), 2);
        assert_eq!(
            reader.pending_depth().expect("other view"),
            0,
            "nothing of an open batch is written yet"
        );

        daemon.commit_batch().expect("commit");
        assert_eq!(reader.pending_depth().expect("other view"), 2);
        assert!(
            reader
                .sync_index(&file("Photos/a.jpg"))
                .expect("read")
                .is_some()
        );

        // With no batch open, each write lands on its own, as before,
        // and a commit with nothing open does nothing.
        daemon.commit_batch().expect("nothing to commit");
        daemon
            .enqueue_intent(&file("c.txt"), PendingIntentKind::Upload, timestamp_ms(3))
            .expect("enqueue");
        assert_eq!(reader.pending_depth().expect("other view"), 3);
    }

    #[test]
    fn a_batch_that_is_never_committed_leaves_the_database_as_it_was() {
        let temp = tempfile::TempDir::new().expect("temp");
        let path = temp.path().join("vapor.sqlite");
        {
            let mut daemon = DurableStateDb::open(&path).expect("open");
            daemon
                .enqueue_intent(
                    Path::new("/tmp/vapor-root/before.txt"),
                    PendingIntentKind::Upload,
                    timestamp_ms(1),
                )
                .expect("enqueue");
            daemon.begin_batch().expect("begin");
            daemon
                .enqueue_intent(
                    Path::new("/tmp/vapor-root/in-the-batch.txt"),
                    PendingIntentKind::Upload,
                    timestamp_ms(2),
                )
                .expect("enqueue");
            // The daemon is killed here: the connection goes with no
            // commit.
        }
        let reopened = DurableStateDb::open(&path).expect("reopen");
        assert_eq!(
            reopened.pending_depth().expect("count"),
            1,
            "the writes of the batch are gone, and nothing else is"
        );
    }

    #[test]
    fn a_writer_waits_for_the_lock_of_another_connection() {
        let temp = tempfile::TempDir::new().expect("temp");
        let path = temp.path().join("vapor.sqlite");
        let mut daemon = DurableStateDb::open(&path).expect("open");
        let mut command = DurableStateDb::open(&path).expect("second connection");
        daemon.begin_batch().expect("begin");
        let (asking, asked) = std::sync::mpsc::channel();
        let writer = std::thread::spawn(move || {
            asking.send(()).expect("signal");
            command.enqueue_intent(
                Path::new("/tmp/vapor-root/answer.txt"),
                PendingIntentKind::Upload,
                timestamp_ms(1),
            )
        });
        // The lock goes while the other connection asks for it or just
        // before: both orders end with its write in the database.
        asked.recv().expect("the writer started");
        daemon.commit_batch().expect("commit");
        writer
            .join()
            .expect("writer thread")
            .expect("the write waited for the lock and landed");
        assert_eq!(daemon.pending_depth().expect("count"), 1);
    }

    #[test]
    fn a_folder_found_under_one_spelling_is_looked_at_again_only_after_an_index_write() {
        let mut database = DurableStateDb::open_in_memory().expect("open");
        let root = PathBuf::from("/tmp/vapor-root");
        let seed = |database: &mut DurableStateDb, path: &Path| {
            database
                .set_sync_index(path, "hash", 1, None, None, "op", timestamp_ms(1))
                .expect("seed");
        };
        let photos = root.join("Photos");
        seed(&mut database, &photos.join("a.jpg"));
        seed(&mut database, &photos.join("2024/b.jpg"));
        let look = |database: &DurableStateDb| {
            database
                .sync_index_row_spelled_otherwise(&photos)
                .expect("look")
        };
        let remembered =
            |database: &DurableStateDb| database.one_spelling.borrow().contains(&photos);

        assert_eq!(look(&database), None);
        assert!(remembered(&database), "the answer needs no second scan");
        assert_eq!(look(&database), None);

        // A row that leaves cannot add a spelling: the answer holds.
        database
            .remove_sync_index(&photos.join("a.jpg"))
            .expect("remove");
        assert!(remembered(&database));

        // A row under another spelling of the folder: the next look
        // finds it.
        seed(&mut database, &root.join("photos/c.jpg"));
        assert!(!remembered(&database));
        assert_eq!(look(&database), Some(root.join("photos/c.jpg")));
        assert!(
            !remembered(&database),
            "a folder with two spellings is never remembered as having one"
        );

        // The rows follow the folder to one spelling, and the look that
        // comes after reads the index again.
        database
            .rekey_sync_index_under(&root.join("photos"), &photos)
            .expect("rekey");
        assert_eq!(look(&database), None);
        assert!(remembered(&database));
        database
            .rekey_sync_index(&photos.join("c.jpg"), &photos.join("C.jpg"))
            .expect("rekey");
        assert!(!remembered(&database));
    }

    #[test]
    fn index_rows_follow_a_file_or_a_folder_to_a_new_spelling() {
        let mut database = DurableStateDb::open_in_memory().expect("open");
        let root = PathBuf::from("/tmp/vapor-root");
        let seed = |database: &mut DurableStateDb, path: &Path| {
            database
                .set_sync_index(path, "hash", 1, None, None, "op", timestamp_ms(1))
                .expect("seed");
        };
        seed(&mut database, &root.join("report.txt"));
        seed(&mut database, &root.join("photos/a.jpg"));
        seed(&mut database, &root.join("photos/2024/b.jpg"));
        seed(&mut database, &root.join("photos-old/c.jpg"));
        seed(&mut database, &root.join("photos.txt"));

        database
            .rekey_sync_index(&root.join("report.txt"), &root.join("Report.txt"))
            .expect("rekey");
        assert!(
            database
                .sync_index(&root.join("report.txt"))
                .expect("read")
                .is_none()
        );
        assert!(
            database
                .sync_index(&root.join("Report.txt"))
                .expect("read")
                .is_some()
        );

        assert!(
            database
                .sync_index_has_rows_under(&root.join("photos"))
                .expect("under")
        );
        assert!(
            !database
                .sync_index_has_rows_under(&root.join("Photos"))
                .expect("under")
        );
        database
            .rekey_sync_index_under(&root.join("photos"), &root.join("Photos"))
            .expect("rekey");
        for moved in ["Photos/a.jpg", "Photos/2024/b.jpg"] {
            assert!(
                database
                    .sync_index(&root.join(moved))
                    .expect("read")
                    .is_some(),
                "{moved}"
            );
        }
        for kept in ["photos-old/c.jpg", "photos.txt"] {
            assert!(
                database
                    .sync_index(&root.join(kept))
                    .expect("read")
                    .is_some(),
                "{kept}"
            );
        }
        assert!(
            !database
                .sync_index_has_rows_under(&root.join("photos"))
                .expect("under")
        );
        assert_eq!(database.sync_index_count().expect("count"), 5);
    }

    #[test]
    fn a_plain_open_refuses_a_database_of_another_schema_version() {
        let temp_dir = TempDir::new().expect("temp dir");
        let database_path = temp_dir.path().join("state/vapor.sqlite");
        if let Some(parent) = database_path.parent() {
            fs::create_dir_all(parent).expect("create parent directory");
        }

        let mut connection = Connection::open(&database_path).expect("open sqlite connection");
        configure_connection(&mut connection).expect("configure connection");
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
    fn a_held_intent_waits_for_its_decision_and_follows_the_answer() {
        let temp_dir = TempDir::new().expect("temp dir");
        let mut db =
            DurableStateDb::open(temp_dir.path().join("state/vapor.sqlite")).expect("open");
        let path = PathBuf::from("/tmp/vapor-root/doomed.txt");
        db.enqueue_intent(&path, PendingIntentKind::Delete, timestamp_ms(0))
            .expect("enqueue");
        let leased = db
            .lease_next_ready(timestamp_ms(1))
            .expect("lease")
            .expect("leased");
        let decision = db
            .create_decision(
                "mass-delete",
                DecisionScope::Batch,
                None,
                "Apply 1 deletion?",
                &[
                    DecisionOption {
                        key: "apply".into(),
                        label: "Apply".into(),
                    },
                    DecisionOption {
                        key: "discard".into(),
                        label: "Discard".into(),
                    },
                ],
                &serde_json::json!({ "count": 1 }),
                timestamp_ms(1),
            )
            .expect("decision");
        assert!(db.hold_leased(leased.id, decision).expect("hold"));

        // Held rows never lease, and count separately from pending.
        assert!(
            db.lease_next_ready(timestamp_ms(100))
                .expect("lease")
                .is_none()
        );
        assert_eq!(db.held_intent_count().expect("held"), 1);
        assert_eq!(db.pending_depth().expect("pending"), 0);
        // Startup recovery leaves held rows alone.
        assert_eq!(db.recover_leased(timestamp_ms(100)).expect("recover"), 0);
        let open = db
            .open_decision("mass-delete", None)
            .expect("open")
            .expect("exists");
        assert_eq!(open.id, decision);
        assert_eq!(open.held_intents, 1);
        assert!(
            db.resolved_unapplied_decisions()
                .expect("unapplied")
                .is_empty()
        );

        // A wrong option is refused; a right one records the answer.
        assert!(
            db.resolve_decision(decision, "maybe", timestamp_ms(200))
                .is_err()
        );
        let resolved = db
            .resolve_decision(decision, "apply", timestamp_ms(200))
            .expect("resolve");
        assert_eq!(resolved.choice.as_deref(), Some("apply"));
        assert!(
            db.resolve_decision(decision, "apply", timestamp_ms(201))
                .is_err()
        );
        assert_eq!(
            db.resolved_unapplied_decisions().expect("unapplied").len(),
            1
        );
        assert_eq!(db.open_decision_count().expect("count"), 0);

        // Applying releases the held intent back to the queue.
        assert_eq!(
            db.release_held(decision, timestamp_ms(300))
                .expect("release"),
            1
        );
        db.mark_decision_applied(decision, timestamp_ms(300))
            .expect("applied");
        assert!(
            db.resolved_unapplied_decisions()
                .expect("unapplied")
                .is_empty()
        );
        let again = db
            .lease_next_ready(timestamp_ms(301))
            .expect("lease")
            .expect("released intent leases");
        assert_eq!(again.id, leased.id);
        assert!(
            again.approved,
            "a released intent carries the user's approval"
        );
    }

    #[test]
    fn dropping_a_held_batch_removes_its_intents() {
        let temp_dir = TempDir::new().expect("temp dir");
        let mut db =
            DurableStateDb::open(temp_dir.path().join("state/vapor.sqlite")).expect("open");
        let decision = db
            .create_decision(
                "mass-delete",
                DecisionScope::Batch,
                None,
                "?",
                &[DecisionOption {
                    key: "discard".into(),
                    label: "Discard".into(),
                }],
                &serde_json::Value::Null,
                timestamp_ms(0),
            )
            .expect("decision");
        for name in ["a", "b"] {
            db.enqueue_intent(
                &PathBuf::from(format!("/tmp/vapor-root/{name}.txt")),
                PendingIntentKind::Delete,
                timestamp_ms(0),
            )
            .expect("enqueue");
            let leased = db
                .lease_next_ready(timestamp_ms(1))
                .expect("lease")
                .expect("row");
            db.hold_leased(leased.id, decision).expect("hold");
        }
        assert_eq!(db.held_intents(decision).expect("held").len(), 2);
        assert_eq!(db.drop_held(decision).expect("drop"), 2);
        assert_eq!(db.queue_depth().expect("depth"), 0);
    }

    #[test]
    fn name_aliases_and_aliased_downloads_round_trip() {
        let temp_dir = TempDir::new().expect("temp dir");
        let mut db =
            DurableStateDb::open(temp_dir.path().join("state/vapor.sqlite")).expect("open");
        let local = PathBuf::from("/tmp/vapor-root/readme~conflict-x.md");
        db.record_name_alias("readme.md", &local, "hash-1", timestamp_ms(0))
            .expect("alias");
        assert_eq!(
            db.name_alias("readme.md").expect("alias"),
            Some((local.clone(), "hash-1".to_string()))
        );
        let id = db
            .enqueue_download_from(&local, "readme.md", timestamp_ms(1))
            .expect("enqueue");
        let leased = db
            .lease_next_ready(timestamp_ms(2))
            .expect("lease")
            .expect("row");
        assert_eq!(leased.id, id);
        assert_eq!(leased.kind, PendingIntentKind::Download);
        assert_eq!(leased.remote_path.as_deref(), Some("readme.md"));
        db.remove_name_alias("readme.md").expect("remove");
        assert_eq!(db.name_alias("readme.md").expect("alias"), None);

        // A folder that changes its spelling takes the aliases of its
        // files with it, on both sides.
        let inside = PathBuf::from("/tmp/vapor-root/Folder/readme~conflict-x.md");
        db.record_name_alias("Folder/Readme.md", &inside, "hash-2", timestamp_ms(3))
            .expect("alias");
        let other = PathBuf::from("/tmp/vapor-root/Folders/a~conflict-x.md");
        db.record_name_alias("Folders/A.md", &other, "hash-3", timestamp_ms(3))
            .expect("alias");
        let moved = db
            .rekey_name_aliases_under(
                Path::new("/tmp/vapor-root/Folder"),
                Path::new("/tmp/vapor-root/FOLDER"),
                "Folder",
                "FOLDER",
            )
            .expect("rekey");
        assert_eq!(moved, 1, "only the aliases under the folder move");
        assert_eq!(
            db.alias_remote_for_local(Path::new("/tmp/vapor-root/FOLDER/readme~conflict-x.md"))
                .expect("alias"),
            Some("FOLDER/Readme.md".to_string())
        );
        assert_eq!(
            db.alias_remote_for_local(&other).expect("alias"),
            Some("Folders/A.md".to_string())
        );
    }

    #[test]
    fn reconcile_backlog_leases_after_fresh_edits_even_when_enqueued_first() {
        let temp_dir = TempDir::new().expect("temp dir");
        let mut state_db = DurableStateDb::open(temp_dir.path().join("state/vapor.sqlite"))
            .expect("open durable state db");
        // A reconcile walk floods the queue first...
        let backlog: Vec<_> = (0..3)
            .map(|index| {
                (
                    PathBuf::from(format!("/tmp/vapor-root/backlog-{index}.bin")),
                    PendingIntentKind::Download,
                    timestamp_ms(100),
                )
            })
            .collect();
        state_db
            .enqueue_intents_coalesced(&backlog, crate::safeguards::IntentSource::ReconcileBacklog)
            .expect("enqueue backlog");
        // ...then the user edits a file (later timestamp AND later id).
        state_db
            .enqueue_intents_coalesced(
                &[(
                    PathBuf::from("/tmp/vapor-root/fresh-edit.rs"),
                    PendingIntentKind::Upload,
                    timestamp_ms(500),
                )],
                crate::safeguards::IntentSource::Fresh,
            )
            .expect("enqueue fresh");

        let first = state_db
            .lease_next_ready(timestamp_ms(1_000))
            .expect("lease")
            .expect("row leased");
        assert_eq!(
            first.path,
            PathBuf::from("/tmp/vapor-root/fresh-edit.rs"),
            "the fresh edit must lease ahead of the earlier reconcile backlog"
        );
    }

    #[test]
    fn fresh_enqueue_promotes_an_existing_backlog_row_out_of_the_backlog_class() {
        let temp_dir = TempDir::new().expect("temp dir");
        let mut state_db = DurableStateDb::open(temp_dir.path().join("state/vapor.sqlite"))
            .expect("open durable state db");
        let path = PathBuf::from("/tmp/vapor-root/shared.rs");
        state_db
            .enqueue_intents_coalesced(
                &[(path.clone(), PendingIntentKind::Upload, timestamp_ms(100))],
                crate::safeguards::IntentSource::ReconcileBacklog,
            )
            .expect("enqueue backlog row");
        // The user edits the same path: the coalesced row must adopt the
        // fresh rank instead of inheriting backlog starvation.
        let inserted = state_db
            .enqueue_intents_coalesced(
                &[(path.clone(), PendingIntentKind::Upload, timestamp_ms(200))],
                crate::safeguards::IntentSource::Fresh,
            )
            .expect("coalesce fresh");
        assert_eq!(inserted, 0, "the fresh enqueue coalesces, not duplicates");

        let leased = state_db
            .lease_next_ready(timestamp_ms(1_000))
            .expect("lease")
            .expect("row leased");
        assert_eq!(leased.path, path);
        assert!(
            leased.priority_rank < crate::safeguards::BACKLOG_INTENT_PRIORITY_RANK,
            "coalescing with fresh work must promote the row's rank"
        );
    }

    #[test]
    fn renew_leases_prevents_the_stale_sweep_from_reclaiming_live_work() {
        let temp_dir = TempDir::new().expect("temp dir");
        let database_path = temp_dir.path().join("state/vapor.sqlite");
        let mut database = DurableStateDb::open(&database_path).expect("open durable state db");
        let path = PathBuf::from("/tmp/vapor-root/big.bin");
        database
            .enqueue_intent(&path, PendingIntentKind::Upload, timestamp_ms(0))
            .expect("enqueue");
        let leased = database
            .lease_next_ready(timestamp_ms(0))
            .expect("lease")
            .expect("leased record");

        let lease_timeout = constants::engine::LEASE_TIMEOUT_MILLIS;
        // Renew the lease just before the timeout would elapse.
        database
            .renew_leases(&[leased.id], timestamp_ms(lease_timeout))
            .expect("renew");
        // A sweep at timeout + 2s must NOT reclaim it: renewed_at + timeout
        // is still in the future.
        let recovered = database
            .recover_stale_leases(timestamp_ms(lease_timeout + 2_000))
            .expect("sweep");
        assert_eq!(recovered, 0, "renewed lease must not be reclaimed");

        // A much later sweep with no further renewal does reclaim it, and
        // keeps the retry history (attempt_count unchanged).
        let recovered = database
            .recover_stale_leases(timestamp_ms(lease_timeout * 3))
            .expect("late sweep");
        assert_eq!(recovered, 1);
        let requeued = database
            .intent_record(leased.id)
            .expect("record")
            .expect("still present");
        assert_eq!(requeued.attempt_count, leased.attempt_count);
    }

    #[test]
    fn prune_failed_intents_drops_rows_past_the_retention_window() {
        let temp_dir = TempDir::new().expect("temp dir");
        let database_path = temp_dir.path().join("state/vapor.sqlite");
        let mut database = DurableStateDb::open(&database_path).expect("open durable state db");

        let path = PathBuf::from("/tmp/vapor-root/old.txt");
        database
            .enqueue_intent(&path, PendingIntentKind::Upload, timestamp_ms(0))
            .expect("enqueue");
        let leased = database
            .lease_next_ready(timestamp_ms(0))
            .expect("lease")
            .expect("leased");
        database
            .finalize_leased_failure(
                leased.id,
                RetryFailureKind::Permanent,
                "boom",
                timestamp_ms(0),
            )
            .expect("finalize");
        assert_eq!(database.failed_depth().expect("failed depth"), 1);

        let retention = constants::state::FAILED_INTENT_RETENTION_MILLIS;
        let pruned = database
            .prune_failed_intents(timestamp_ms(retention + 1))
            .expect("prune");
        assert_eq!(pruned, 1);
        assert_eq!(database.failed_depth().expect("failed depth"), 0);
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
    fn the_daemon_sets_a_database_of_another_schema_version_aside_and_starts_fresh() {
        let temp_dir = TempDir::new().expect("temp dir");
        let database_path = temp_dir.path().join("state/vapor.sqlite");
        if let Some(parent) = database_path.parent() {
            fs::create_dir_all(parent).expect("create parent directory");
        }
        let mut connection = Connection::open(&database_path).expect("open sqlite connection");
        configure_connection(&mut connection).expect("configure connection");
        connection
            .execute_batch(
                "CREATE TABLE schema_meta (
                     singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
                     schema_version INTEGER NOT NULL CHECK(schema_version > 0)
                 );
                 INSERT INTO schema_meta (singleton, schema_version) VALUES (1, 6);",
            )
            .expect("seed an older schema");
        drop(connection);

        let fresh =
            DurableStateDb::open_with_corruption_recovery(&database_path, timestamp_ms(1_000))
                .expect("another schema version must not stop the daemon");
        assert_eq!(
            fresh.schema_version().expect("version"),
            CURRENT_SCHEMA_VERSION
        );
        assert_eq!(fresh.queue_depth().expect("depth"), 0);
        assert!(
            database_path
                .with_extension("sqlite.schema-v6-1000")
                .exists(),
            "the old database stays next to the new one"
        );
    }

    #[test]
    fn databases_set_aside_are_removed_after_the_retention_window() {
        let temp_dir = TempDir::new().expect("temp dir");
        let database_path = temp_dir.path().join("state/vapor.sqlite");
        fs::create_dir_all(database_path.parent().unwrap()).expect("parent");
        let old = database_path.with_extension("sqlite.corrupt-1000");
        let recent_ms = QUARANTINE_RETENTION.as_millis() as u64;
        let recent = database_path.with_extension(format!("sqlite.schema-v6-{recent_ms}"));
        fs::write(&old, b"old").expect("seed");
        fs::write(&recent, b"recent").expect("seed");

        DurableStateDb::open_with_corruption_recovery(
            &database_path,
            timestamp_ms(recent_ms + 2_000),
        )
        .expect("open");
        assert!(!old.exists(), "past the window");
        assert!(recent.exists(), "inside the window");
    }

    #[test]
    fn a_damaged_page_is_found_at_the_daemon_open() {
        let temp_dir = TempDir::new().expect("temp dir");
        let database_path = temp_dir.path().join("state/vapor.sqlite");
        {
            let mut database = DurableStateDb::open(&database_path).expect("open");
            for index in 0..400 {
                database
                    .enqueue_intent(
                        &PathBuf::from(format!("/tmp/vapor-root/file-{index}.txt")),
                        PendingIntentKind::Upload,
                        timestamp_ms(1),
                    )
                    .expect("enqueue");
            }
            database
                .connection
                .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
                .expect("checkpoint");
        }
        // Overwrite a page in the middle of the file: the header stays
        // valid, so the open itself succeeds.
        let mut bytes = fs::read(&database_path).expect("read");
        assert!(bytes.len() > 4096 * 4, "the file has pages to damage");
        let middle = (bytes.len() / 4096 / 2) * 4096;
        for byte in &mut bytes[middle..middle + 4096] {
            *byte = 0xA5;
        }
        fs::write(&database_path, &bytes).expect("damage");

        let fresh =
            DurableStateDb::open_with_corruption_recovery(&database_path, timestamp_ms(5_000))
                .expect("a damaged database is replaced");
        assert_eq!(fresh.queue_depth().expect("depth"), 0);
        assert!(database_path.with_extension("sqlite.corrupt-5000").exists());
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

        let mut connection = Connection::open(&database_path).expect("open sqlite connection");
        configure_connection(&mut connection).expect("configure connection");
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
            .enqueue_intents_coalesced(
                &[
                    (path.clone(), PendingIntentKind::Upload, timestamp_ms(100)),
                    (path.clone(), PendingIntentKind::Upload, timestamp_ms(200)),
                ],
                crate::safeguards::IntentSource::Fresh,
            )
            .expect("coalesced enqueue");
        assert_eq!(inserted, 1);
        assert_eq!(database.pending_depth().expect("pending depth"), 1);

        // A second batch for the same still-pending path coalesces too.
        let inserted = database
            .enqueue_intents_coalesced(
                &[(path.clone(), PendingIntentKind::Upload, timestamp_ms(300))],
                crate::safeguards::IntentSource::Fresh,
            )
            .expect("coalesced enqueue");
        assert_eq!(inserted, 0);

        // A different kind for the same path is separate work.
        let inserted = database
            .enqueue_intents_coalesced(
                &[(path.clone(), PendingIntentKind::Delete, timestamp_ms(400))],
                crate::safeguards::IntentSource::Fresh,
            )
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
            .enqueue_intents_coalesced(
                &[(path.clone(), PendingIntentKind::Upload, timestamp_ms(200))],
                crate::safeguards::IntentSource::Fresh,
            )
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

    #[test]
    fn unresolved_conflict_copies_count_indexed_and_queued_copies_once() {
        let temp_dir = TempDir::new().expect("temp dir");
        let mut db =
            DurableStateDb::open(temp_dir.path().join("state/vapor.sqlite")).expect("open");
        let now = timestamp_ms(1_700_000_000_000);
        assert_eq!(
            db.conflict_copy_paths()
                .map(|paths| paths.len())
                .expect("count"),
            0
        );

        let root = temp_dir.path().join("local");
        let synced_copy = root.join("report~conflict-mbp-1700000000000.md");
        let queued_copy = root.join("notes~conflict-mbp-1700000000001.txt");
        // Marker text without the machine grammar (no epoch timestamp)
        // is a user file, never a conflict.
        let lookalike = root.join("draft~conflict-notes.txt");
        for path in [&root.join("report.md"), &synced_copy, &lookalike] {
            db.set_sync_index(path, "hash", 1, None, None, "op", now)
                .expect("index");
        }
        db.enqueue_intent(&queued_copy, PendingIntentKind::Upload, now)
            .expect("queue upload");
        // Queued and indexed at once still counts as one copy.
        db.enqueue_intent(&synced_copy, PendingIntentKind::Upload, now)
            .expect("queue upload");
        // A queued deletion is the user resolving; only an upload
        // announces a copy the index has not seen.
        db.enqueue_intent(
            &root.join("old~conflict-mbp-1700000000002.txt"),
            PendingIntentKind::Delete,
            now,
        )
        .expect("queue delete");
        assert_eq!(
            db.conflict_copy_paths()
                .map(|paths| paths.len())
                .expect("count"),
            2
        );

        db.remove_sync_index(&synced_copy).expect("remove");
        // Still queued for upload, so still announced.
        assert_eq!(
            db.conflict_copy_paths()
                .map(|paths| paths.len())
                .expect("count"),
            2
        );
        for leased in db.lease_ready_batch(now, 8).expect("lease") {
            assert!(db.complete_leased(leased.id).expect("complete"));
        }
        assert_eq!(
            db.conflict_copy_paths()
                .map(|paths| paths.len())
                .expect("count"),
            0
        );
    }

    #[test]
    fn unresolved_conflict_copy_query_reads_only_the_partial_indexes() {
        let marker = crate::conflict::CONFLICT_MARKER;
        assert!(
            UNRESOLVED_CONFLICT_COPY_QUERY.contains(&format!("LIKE '%{marker}%'")),
            "the query must match on the conflict marker the copy paths carry"
        );

        let temp_dir = TempDir::new().expect("temp dir");
        let db = DurableStateDb::open(temp_dir.path().join("state/vapor.sqlite")).expect("open");
        let mut statement = db
            .connection
            .prepare(&format!(
                "EXPLAIN QUERY PLAN {UNRESOLVED_CONFLICT_COPY_QUERY}"
            ))
            .expect("plan");
        let plan: Vec<String> = statement
            .query_map([], |row| row.get::<_, String>(3))
            .expect("plan rows")
            .map(|row| row.expect("plan row"))
            .collect();
        let joined = plan.join("\n");
        for index in [
            "idx_sync_index_conflict_copy",
            "idx_queue_intents_conflict_copy",
        ] {
            assert!(
                joined.contains(&format!("USING COVERING INDEX {index}")),
                "the count must stay a partial-index read, not a table scan: {joined}"
            );
        }
    }

    fn timestamp_ms(milliseconds: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_millis(milliseconds)
    }
}
