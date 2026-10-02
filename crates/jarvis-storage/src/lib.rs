#![forbid(unsafe_code)]

//! Narrow local persistence for explicit, user-provided preference mappings
//! and corrections. The database connection and SQL remain private; the schema
//! has no transcript- or credential-specific fields, and this slice stores no
//! skills, grants, or trust. Its role-specific identifiers are not a secret
//! vault: trusted callers must never encode credentials or other secrets as IDs.
//!
//! Production opens are pinned to `%LOCALAPPDATA%\\Jarvis\\jarvis.db`. The
//! caller must still serialize access through the authoritative supervisor
//! and deny all storage activity while Competitive Mode is active.

use std::env;
use std::error::Error;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::backup::Backup;
use rusqlite::{params, Connection, OpenFlags, OptionalExtension, TransactionBehavior};
use sha2::{Digest, Sha256};

const DATABASE_FILE: &str = "jarvis.db";
const PRE_V2_BACKUP_FILE: &str = "jarvis.db.pre-v2.bak";
const DATA_DIRECTORY: &str = "Jarvis";
const MAX_IDENTIFIER_BYTES: usize = 128;
const MAX_STORED_RECORDS: i64 = 8_192;
const BUSY_TIMEOUT_MS: u64 = 250;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StorageError {
    LocalAppDataUnavailable,
    LocalAppDataNotAbsolute,
    InvalidIdentifier,
    ClockBeforeUnixEpoch,
    NewerSchemaVersion(u32),
    MigrationHistoryGap(u32),
    MigrationNameMismatch(u32),
    MigrationChecksumMismatch(u32),
    UnversionedDatabase,
    BackupAlreadyExists,
    BackupFailed(String),
    IdempotencyConflict,
    PreferenceForgotten,
    StalePreferenceRevision,
    CapacityReached,
    Sqlite(String),
}

impl fmt::Display for StorageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LocalAppDataUnavailable => formatter.write_str("LOCALAPPDATA is unavailable"),
            Self::LocalAppDataNotAbsolute => {
                formatter.write_str("LOCALAPPDATA must be an absolute path")
            }
            Self::InvalidIdentifier => formatter
                .write_str("identifier must be 1-128 ASCII letters, digits, '.', '_', '-', or ':'"),
            Self::ClockBeforeUnixEpoch => {
                formatter.write_str("system clock is before the Unix epoch")
            }
            Self::NewerSchemaVersion(version) => write!(
                formatter,
                "database schema version {version} is newer than this Jarvis build"
            ),
            Self::MigrationHistoryGap(version) => {
                write!(
                    formatter,
                    "database migration history is missing version {version}"
                )
            }
            Self::MigrationNameMismatch(version) => {
                write!(
                    formatter,
                    "database migration name mismatch at version {version}"
                )
            }
            Self::MigrationChecksumMismatch(version) => write!(
                formatter,
                "database migration checksum mismatch at version {version}"
            ),
            Self::UnversionedDatabase => formatter.write_str(
                "database contains tables but no Jarvis migration history; refusing to modify it",
            ),
            Self::BackupAlreadyExists => formatter.write_str(
                "pre-v2 Jarvis database backup already exists; refusing to overwrite it or migrate",
            ),
            Self::BackupFailed(message) => write!(
                formatter,
                "could not create a verified pre-v2 database backup; migration was not run: {message}"
            ),
            Self::IdempotencyConflict => {
                formatter.write_str("event ID was already used for a different explicit record")
            }
            Self::PreferenceForgotten => formatter.write_str(
                "preference was forgotten; restoring it requires the current tombstone event ID",
            ),
            Self::StalePreferenceRevision => formatter.write_str(
                "preference changed since the supplied event ID; no update was recorded",
            ),
            Self::CapacityReached => {
                formatter.write_str("explicit memory storage reached its fixed record limit")
            }
            Self::Sqlite(message) => write!(formatter, "SQLite storage error: {message}"),
        }
    }
}

impl Error for StorageError {}

impl From<rusqlite::Error> for StorageError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error.to_string())
    }
}

/// Private common representation; public APIs use role-specific newtypes.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct Identifier(String);

impl Identifier {
    fn new(value: impl Into<String>) -> Result<Self, StorageError> {
        let value = value.into();
        if value.is_empty()
            || value.len() > MAX_IDENTIFIER_BYTES
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._:-".contains(&byte))
        {
            return Err(StorageError::InvalidIdentifier);
        }
        Ok(Self(value))
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

macro_rules! define_identifier_type {
    ($name:ident, $description:literal) => {
        #[doc = $description]
        #[derive(Clone, Debug, Eq, Hash, PartialEq)]
        pub struct $name(Identifier);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, StorageError> {
                Identifier::new(value).map(Self)
            }

            #[must_use]
            pub fn as_str(&self) -> &str {
                self.0.as_str()
            }
        }
    };
}

define_identifier_type!(EventId, "Stable bounded ID for one explicit user event.");
define_identifier_type!(
    PreferenceKey,
    "Stable bounded key for an explicit preference."
);
define_identifier_type!(
    ReferenceId,
    "Stable bounded non-secret target reference ID."
);
define_identifier_type!(
    CorrectionTerm,
    "Stable bounded single-token correction term."
);
define_identifier_type!(CanonicalId, "Stable bounded canonical target or intent ID.");

/// Result of inserting an explicit preference or correction event.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecordOutcome {
    Inserted,
    AlreadyRecorded,
}

/// The latest explicit preference value stored for a key.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredPreference {
    pub event_id: EventId,
    pub key: PreferenceKey,
    pub target: ReferenceId,
    pub created_at_ms: i64,
}

/// The active revocation marker for one forgotten preference. The old value
/// is intentionally not returned; `superseded_event_id` is an audit anchor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredPreferenceTombstone {
    pub event_id: EventId,
    pub key: PreferenceKey,
    pub superseded_event_id: EventId,
    pub created_at_ms: i64,
}

/// The latest explicit correction stored for a term.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredCorrection {
    pub event_id: EventId,
    pub term: CorrectionTerm,
    pub canonical_id: CanonicalId,
    pub created_at_ms: i64,
}

#[derive(Clone, Copy)]
struct Migration {
    version: u32,
    name: &'static str,
    sql: &'static str,
}

const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "explicit_records",
        sql: include_str!("../migrations/0001_explicit_records.sql"),
    },
    Migration {
        version: 2,
        name: "preference_tombstones",
        sql: include_str!("../migrations/0002_preference_tombstones.sql"),
    },
];

/// The sole storage handle. Its connection is intentionally not exposed or
/// cloneable; the supervisor should own one handle and serialize all calls.
pub struct Store {
    connection: Connection,
}

impl Store {
    /// Opens the fixed production database under the current user's LocalAppData.
    /// No path or database filename can be supplied by configuration or a worker.
    pub fn open_local_app_data() -> Result<Self, StorageError> {
        let local_app_data = env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .ok_or(StorageError::LocalAppDataUnavailable)?;
        if !local_app_data.is_absolute() {
            return Err(StorageError::LocalAppDataNotAbsolute);
        }
        let data_directory = local_app_data.join(DATA_DIRECTORY);
        fs::create_dir_all(&data_directory).map_err(|error| {
            StorageError::Sqlite(format!("could not create Jarvis data directory: {error}"))
        })?;
        Self::open_database(data_directory.join(DATABASE_FILE))
    }

    /// Test-only constructor for isolated temporary database roots. Not
    /// available in production builds unless the explicit `test-util` feature
    /// is enabled by an integration-test target.
    #[cfg(feature = "test-util")]
    pub fn open_in_test_root(root: impl AsRef<std::path::Path>) -> Result<Self, StorageError> {
        let root = root.as_ref();
        fs::create_dir_all(root).map_err(|error| {
            StorageError::Sqlite(format!("could not create test database directory: {error}"))
        })?;
        Self::open_database(root.join(DATABASE_FILE))
    }

    fn open_database(path: PathBuf) -> Result<Self, StorageError> {
        let mut connection = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
        )?;
        connection.busy_timeout(std::time::Duration::from_millis(BUSY_TIMEOUT_MS))?;
        connection.pragma_update(None, "trusted_schema", "OFF")?;
        backup_before_v2_if_needed(&connection, &path)?;
        connection.pragma_update(None, "journal_mode", "DELETE")?;
        let journal_mode: String =
            connection.pragma_query_value(None, "journal_mode", |row| row.get(0))?;
        if !journal_mode.eq_ignore_ascii_case("delete") {
            return Err(StorageError::Sqlite(
                "database did not enter rollback-journal mode".to_owned(),
            ));
        }
        apply_migrations(&mut connection, MIGRATIONS)?;
        Ok(Self { connection })
    }

    /// Appends one explicit key-to-reference preference. `event_id` retries
    /// are idempotent; reusing an event ID with different contents is denied.
    pub fn record_preference(
        &mut self,
        event_id: &EventId,
        key: &PreferenceKey,
        target: &ReferenceId,
    ) -> Result<RecordOutcome, StorageError> {
        self.record(
            "preference",
            event_id.as_str(),
            key.as_str(),
            target.as_str(),
            None,
        )
    }

    /// Appends a tombstone for the exact preference revision the user chose to
    /// forget. The prior value remains in the local audit history; this does
    /// not claim physical erasure from the database or backups. A stale view
    /// cannot forget a newer value. A repeated event ID is idempotent.
    pub fn forget_preference(
        &mut self,
        event_id: &EventId,
        key: &PreferenceKey,
        expected_current_event_id: &EventId,
    ) -> Result<RecordOutcome, StorageError> {
        self.record(
            "preference_forget",
            event_id.as_str(),
            key.as_str(),
            "forgotten",
            Some(expected_current_event_id.as_str()),
        )
    }

    /// Explicitly restores a forgotten preference. The caller must present
    /// the exact current tombstone event ID; an old save or old restore cannot
    /// cross a later forget boundary. Normal `record_preference` stays closed
    /// for a key that has ever been forgotten.
    pub fn restore_preference(
        &mut self,
        event_id: &EventId,
        key: &PreferenceKey,
        target: &ReferenceId,
        expected_tombstone_event_id: &EventId,
    ) -> Result<RecordOutcome, StorageError> {
        self.record(
            "preference_restore",
            event_id.as_str(),
            key.as_str(),
            target.as_str(),
            Some(expected_tombstone_event_id.as_str()),
        )
    }

    /// Appends one explicit single-token correction mapping. This stores no
    /// utterance/transcript text; both fields must be stable identifiers.
    pub fn record_correction(
        &mut self,
        event_id: &EventId,
        term: &CorrectionTerm,
        canonical_id: &CanonicalId,
    ) -> Result<RecordOutcome, StorageError> {
        self.record(
            "correction",
            event_id.as_str(),
            term.as_str(),
            canonical_id.as_str(),
            None,
        )
    }

    /// Returns the active preference for a key, or `None` after a tombstone.
    pub fn preference(
        &self,
        key: &PreferenceKey,
    ) -> Result<Option<StoredPreference>, StorageError> {
        let row = self
            .connection
            .query_row(
                "SELECT event_id, record_kind, record_key, record_value, created_at_ms \
                 FROM explicit_records \
                 WHERE record_kind IN \
                     ('preference', 'preference_forget', 'preference_restore') \
                   AND record_key = ?1 \
                 ORDER BY sequence DESC LIMIT 1",
                [key.as_str()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                    ))
                },
            )
            .optional()?;
        row.filter(|(_, kind, _, _, _)| kind != "preference_forget")
            .map(|(event_id, _, key, value, created_at_ms)| {
                Ok(StoredPreference {
                    event_id: EventId::new(event_id)?,
                    key: PreferenceKey::new(key)?,
                    target: ReferenceId::new(value)?,
                    created_at_ms,
                })
            })
            .transpose()
    }

    /// Returns the current tombstone, if the latest event for this key is a
    /// forget event. This supplies the exact revision token for an explicit
    /// future restore without revealing the retained old preference value.
    pub fn preference_tombstone(
        &self,
        key: &PreferenceKey,
    ) -> Result<Option<StoredPreferenceTombstone>, StorageError> {
        let row = self
            .connection
            .query_row(
                "SELECT event_id, record_kind, record_key, causal_event_id, created_at_ms \
                 FROM explicit_records \
                 WHERE record_kind IN \
                     ('preference', 'preference_forget', 'preference_restore') \
                   AND record_key = ?1 \
                 ORDER BY sequence DESC LIMIT 1",
                [key.as_str()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, i64>(4)?,
                    ))
                },
            )
            .optional()?;
        row.filter(|(_, kind, _, _, _)| kind == "preference_forget")
            .map(|(event_id, _, key, superseded_event_id, created_at_ms)| {
                Ok(StoredPreferenceTombstone {
                    event_id: EventId::new(event_id)?,
                    key: PreferenceKey::new(key)?,
                    superseded_event_id: EventId::new(
                        superseded_event_id.ok_or(StorageError::StalePreferenceRevision)?,
                    )?,
                    created_at_ms,
                })
            })
            .transpose()
    }

    /// Returns the most recently appended correction for a single-token term.
    pub fn correction(
        &self,
        term: &CorrectionTerm,
    ) -> Result<Option<StoredCorrection>, StorageError> {
        let row = self
            .connection
            .query_row(
                "SELECT event_id, record_key, record_value, created_at_ms \
                 FROM explicit_records \
                 WHERE record_kind = 'correction' AND record_key = ?1 \
                 ORDER BY sequence DESC LIMIT 1",
                [term.as_str()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                },
            )
            .optional()?;
        row.map(|(event_id, term, canonical_value, created_at_ms)| {
            Ok(StoredCorrection {
                event_id: EventId::new(event_id)?,
                term: CorrectionTerm::new(term)?,
                canonical_id: CanonicalId::new(canonical_value)?,
                created_at_ms,
            })
        })
        .transpose()
    }

    fn record(
        &mut self,
        kind: &'static str,
        event_id: &str,
        key: &str,
        value: &str,
        causal_event_id: Option<&str>,
    ) -> Result<RecordOutcome, StorageError> {
        let created_at_ms = unix_time_millis()?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = tx
            .query_row(
                "SELECT record_kind, record_key, record_value, causal_event_id \
                 FROM explicit_records WHERE event_id = ?1",
                [event_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                    ))
                },
            )
            .optional()?;
        if let Some((existing_kind, existing_key, existing_value, existing_causal)) = existing {
            if existing_kind == kind
                && existing_key == key
                && existing_value == value
                && existing_causal.as_deref() == causal_event_id
            {
                return Ok(RecordOutcome::AlreadyRecorded);
            }
            return Err(StorageError::IdempotencyConflict);
        }

        match kind {
            "preference" => {
                let has_forget: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM explicit_records \
                     WHERE record_kind = 'preference_forget' AND record_key = ?1)",
                    [key],
                    |row| row.get(0),
                )?;
                if has_forget {
                    return Err(StorageError::PreferenceForgotten);
                }
            }
            "preference_forget" => {
                let current = latest_preference_state(&tx, key)?;
                if !matches!(
                    current,
                    Some((ref current_id, ref current_kind))
                        if Some(current_id.as_str()) == causal_event_id
                            && current_kind != "preference_forget"
                ) {
                    return Err(StorageError::StalePreferenceRevision);
                }
            }
            "preference_restore" => {
                let current = latest_preference_state(&tx, key)?;
                if !matches!(
                    current,
                    Some((ref current_id, ref current_kind))
                        if Some(current_id.as_str()) == causal_event_id
                            && current_kind == "preference_forget"
                ) {
                    return Err(StorageError::StalePreferenceRevision);
                }
            }
            "correction" => {}
            _ => unreachable!("only fixed record kinds reach storage"),
        }

        // Records are append-only in this API, so the largest sequence is an
        // indexed capacity check. The immediate transaction serializes this
        // check with the following insert.
        let largest_sequence: i64 = tx.query_row(
            "SELECT COALESCE(MAX(sequence), 0) FROM explicit_records",
            [],
            |row| row.get(0),
        )?;
        if largest_sequence >= MAX_STORED_RECORDS {
            return Err(StorageError::CapacityReached);
        }

        tx.execute(
            "INSERT INTO explicit_records \
             (event_id, record_kind, record_key, record_value, causal_event_id, created_at_ms) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![event_id, kind, key, value, causal_event_id, created_at_ms],
        )?;
        tx.commit()?;
        Ok(RecordOutcome::Inserted)
    }
}

/// The online backup API reads a transactionally consistent snapshot even if
/// an old database is in WAL mode. The fixed sibling path is reserved with
/// `create_new`, so an existing backup is never overwritten. Any failure
/// leaves the source at v1; an incomplete reserved backup is intentionally
/// retained for inspection instead of being mistaken for a valid backup.
fn backup_before_v2_if_needed(
    connection: &Connection,
    source_path: &Path,
) -> Result<(), StorageError> {
    if !table_exists(connection, "schema_migrations")? {
        return Ok(());
    }
    let history = read_migration_history(connection)?;
    if history.len() != 1 || history[0].0 != 1 {
        return Ok(());
    }
    let v1 = &MIGRATIONS[0];
    if history[0].1 != v1.name || history[0].2 != migration_checksum(v1) {
        // The standard migration validator will return the precise mismatch;
        // do not create a backup of a schema this build cannot authenticate.
        return Ok(());
    }

    let backup_path = source_path.with_file_name(PRE_V2_BACKUP_FILE);
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&backup_path)
    {
        Ok(file) => drop(file),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(StorageError::BackupAlreadyExists);
        }
        Err(error) => return Err(StorageError::BackupFailed(error.to_string())),
    }

    let mut destination =
        Connection::open_with_flags(&backup_path, OpenFlags::SQLITE_OPEN_READ_WRITE)
            .map_err(|error| StorageError::BackupFailed(error.to_string()))?;
    destination
        .busy_timeout(Duration::from_millis(BUSY_TIMEOUT_MS))
        .map_err(|error| StorageError::BackupFailed(error.to_string()))?;
    Backup::new(connection, &mut destination)
        .and_then(|backup| backup.run_to_completion(64, Duration::from_millis(10), None))
        .map_err(|error| StorageError::BackupFailed(error.to_string()))?;

    let integrity: String = destination
        .pragma_query_value(None, "integrity_check", |row| row.get(0))
        .map_err(|error| StorageError::BackupFailed(error.to_string()))?;
    if integrity != "ok" {
        return Err(StorageError::BackupFailed(format!(
            "backup integrity check returned {integrity}"
        )));
    }
    let backup_history = read_migration_history(&destination)
        .map_err(|error| StorageError::BackupFailed(error.to_string()))?;
    if backup_history != history {
        return Err(StorageError::BackupFailed(
            "backup migration history differs from source".to_owned(),
        ));
    }
    Ok(())
}

fn latest_preference_state(
    connection: &Connection,
    key: &str,
) -> Result<Option<(String, String)>, StorageError> {
    connection
        .query_row(
            "SELECT event_id, record_kind FROM explicit_records \
             WHERE record_kind IN ('preference', 'preference_forget', 'preference_restore') \
               AND record_key = ?1 ORDER BY sequence DESC LIMIT 1",
            [key],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(Into::into)
}

fn unix_time_millis() -> Result<i64, StorageError> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| StorageError::ClockBeforeUnixEpoch)?;
    i64::try_from(duration.as_millis()).map_err(|_| StorageError::ClockBeforeUnixEpoch)
}

fn apply_migrations(
    connection: &mut Connection,
    migrations: &[Migration],
) -> Result<(), StorageError> {
    let has_ledger = table_exists(connection, "schema_migrations")?;
    if !has_ledger && has_user_tables(connection)? {
        return Err(StorageError::UnversionedDatabase);
    }

    let applied = if has_ledger {
        read_migration_history(connection)?
    } else {
        Vec::new()
    };

    for (index, (version, name, checksum)) in applied.iter().enumerate() {
        let Some(migration) = migrations.get(index) else {
            return Err(StorageError::NewerSchemaVersion(*version));
        };
        let expected_version = index as u32 + 1;
        if *version != expected_version {
            return Err(StorageError::MigrationHistoryGap(expected_version));
        }
        if *version != migration.version || name != migration.name {
            return Err(StorageError::MigrationNameMismatch(*version));
        }
        if checksum != &migration_checksum(migration) {
            return Err(StorageError::MigrationChecksumMismatch(*version));
        }
    }

    if let Some((version, _, _)) = applied.last() {
        if *version > migrations.last().map_or(0, |migration| migration.version) {
            return Err(StorageError::NewerSchemaVersion(*version));
        }
    }

    for migration in migrations.iter().skip(applied.len()) {
        run_migration(connection, migration)?;
    }
    Ok(())
}

fn run_migration(connection: &mut Connection, migration: &Migration) -> Result<(), StorageError> {
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute_batch(migration.sql)?;
    tx.execute(
        "INSERT INTO schema_migrations(version, name, checksum, applied_at_ms) \
         VALUES (?1, ?2, ?3, ?4)",
        params![
            migration.version,
            migration.name,
            migration_checksum(migration),
            unix_time_millis()?
        ],
    )?;
    tx.commit()?;
    Ok(())
}

fn read_migration_history(
    connection: &Connection,
) -> Result<Vec<(u32, String, String)>, StorageError> {
    let mut statement = connection
        .prepare("SELECT version, name, checksum FROM schema_migrations ORDER BY version ASC")?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, u32>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

fn table_exists(connection: &Connection, table: &str) -> Result<bool, StorageError> {
    connection
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [table],
            |_| Ok(()),
        )
        .optional()
        .map(|value| value.is_some())
        .map_err(Into::into)
}

fn has_user_tables(connection: &Connection) -> Result<bool, StorageError> {
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master \
             WHERE type = 'table' AND name NOT LIKE 'sqlite_%')",
            [],
            |row| row.get(0),
        )
        .map_err(Into::into)
}

fn migration_checksum(migration: &Migration) -> String {
    let digest = Sha256::digest(migration.sql.as_bytes());
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use fmt::Write as _;
        let _ = write!(encoded, "{byte:02x}");
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEST_DIR: AtomicU64 = AtomicU64::new(1);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            loop {
                let unique = NEXT_TEST_DIR.fetch_add(1, Ordering::Relaxed);
                let path = env::temp_dir().join(format!(
                    "jarvis-storage-test-{}-{unique}",
                    std::process::id()
                ));
                match fs::create_dir(&path) {
                    Ok(()) => return Self(path),
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(error) => panic!("create isolated test directory: {error}"),
                }
            }
        }

        fn database_path(&self) -> PathBuf {
            self.0.join(DATABASE_FILE)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn test_store(directory: &TestDirectory) -> Store {
        Store::open_database(directory.database_path()).expect("open test store")
    }

    fn event_id(value: &str) -> EventId {
        EventId::new(value).expect("valid event ID")
    }

    fn preference_key(value: &str) -> PreferenceKey {
        PreferenceKey::new(value).expect("valid preference key")
    }

    fn reference_id(value: &str) -> ReferenceId {
        ReferenceId::new(value).expect("valid reference ID")
    }

    fn correction_term(value: &str) -> CorrectionTerm {
        CorrectionTerm::new(value).expect("valid correction term")
    }

    fn canonical_id(value: &str) -> CanonicalId {
        CanonicalId::new(value).expect("valid canonical ID")
    }

    #[test]
    fn fresh_database_has_only_schema_and_explicit_records() {
        let directory = TestDirectory::new();
        let store = test_store(&directory);
        let tables = {
            let mut statement = store
                .connection
                .prepare(
                    "SELECT name FROM sqlite_master WHERE type = 'table' \
                 AND name NOT LIKE 'sqlite_%' ORDER BY name",
                )
                .expect("prepare table query");
            statement
                .query_map([], |row| row.get::<_, String>(0))
                .expect("query tables")
                .collect::<Result<Vec<_>, _>>()
                .expect("collect tables")
        };
        assert_eq!(tables, ["explicit_records", "schema_migrations"]);
        assert_eq!(read_migration_history(&store.connection).unwrap().len(), 2);
        let record_columns = {
            let mut statement = store
                .connection
                .prepare("PRAGMA table_info(explicit_records)")
                .expect("prepare record columns query");
            statement
                .query_map([], |row| row.get::<_, String>(1))
                .expect("query record columns")
                .collect::<Result<Vec<_>, _>>()
                .expect("collect record columns")
        };
        assert_eq!(
            record_columns,
            [
                "sequence",
                "event_id",
                "record_kind",
                "record_key",
                "record_value",
                "causal_event_id",
                "created_at_ms"
            ]
        );
        let wal_mode: String = store
            .connection
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .unwrap();
        assert_eq!(wal_mode.to_ascii_lowercase(), "delete");
    }

    #[test]
    fn migration_rerun_and_reopen_preserve_records_without_duplicate_migration() {
        let directory = TestDirectory::new();
        let key = preference_key("preferred_browser");
        {
            let mut store = test_store(&directory);
            assert_eq!(
                store
                    .record_preference(&event_id("event-1"), &key, &reference_id("chrome"))
                    .unwrap(),
                RecordOutcome::Inserted
            );
        }

        let mut connection = Connection::open(directory.database_path()).unwrap();
        apply_migrations(&mut connection, MIGRATIONS).unwrap();
        assert_eq!(read_migration_history(&connection).unwrap().len(), 2);
        drop(connection);

        let store = test_store(&directory);
        let stored = store.preference(&key).unwrap().unwrap();
        assert_eq!(stored.event_id, event_id("event-1"));
        assert_eq!(stored.target, reference_id("chrome"));
    }

    #[test]
    fn version_one_database_upgrades_without_losing_explicit_records() {
        let directory = TestDirectory::new();
        let mut connection = Connection::open(directory.database_path()).unwrap();
        apply_migrations(&mut connection, &MIGRATIONS[..1]).unwrap();
        connection
            .execute(
                "INSERT INTO explicit_records \
                 (event_id, record_kind, record_key, record_value, created_at_ms) \
                 VALUES ('legacy-pref', 'preference', 'preferred_browser', 'chrome', 1)",
                [],
            )
            .unwrap();
        drop(connection);

        let mut store = test_store(&directory);
        assert_eq!(read_migration_history(&store.connection).unwrap().len(), 2);
        let backup = Connection::open_with_flags(
            directory.0.join(PRE_V2_BACKUP_FILE),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .expect("verified pre-v2 backup must exist");
        assert_eq!(read_migration_history(&backup).unwrap().len(), 1);
        let original: String = backup
            .query_row(
                "SELECT record_value FROM explicit_records WHERE event_id = 'legacy-pref'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(original, "chrome");
        let key = preference_key("preferred_browser");
        assert_eq!(
            store.preference(&key).unwrap().unwrap().target,
            reference_id("chrome")
        );
        assert_eq!(
            store
                .forget_preference(&event_id("forget-1"), &key, &event_id("legacy-pref"))
                .unwrap(),
            RecordOutcome::Inserted
        );
        assert!(store.preference(&key).unwrap().is_none());
    }

    #[test]
    fn existing_backup_blocks_v1_upgrade_without_changing_source() {
        let directory = TestDirectory::new();
        let mut connection = Connection::open(directory.database_path()).unwrap();
        apply_migrations(&mut connection, &MIGRATIONS[..1]).unwrap();
        drop(connection);
        let backup_path = directory.0.join(PRE_V2_BACKUP_FILE);
        fs::write(&backup_path, b"keep existing backup").unwrap();

        let result = Store::open_database(directory.database_path());
        assert!(matches!(result, Err(StorageError::BackupAlreadyExists)));
        assert_eq!(fs::read(&backup_path).unwrap(), b"keep existing backup");
        let source = Connection::open(directory.database_path()).unwrap();
        assert_eq!(read_migration_history(&source).unwrap().len(), 1);
    }

    #[test]
    fn pre_v2_backup_includes_uncheckpointed_wal_data() {
        let directory = TestDirectory::new();
        let path = directory.database_path();
        let mut source = Connection::open(&path).unwrap();
        apply_migrations(&mut source, &MIGRATIONS[..1]).unwrap();
        source.pragma_update(None, "journal_mode", "WAL").unwrap();
        source
            .execute(
                "INSERT INTO explicit_records \
                 (event_id, record_kind, record_key, record_value, created_at_ms) \
                 VALUES ('wal-pref', 'preference', 'preferred_browser', 'discord', 1)",
                [],
            )
            .unwrap();

        backup_before_v2_if_needed(&source, &path).unwrap();
        let backup = Connection::open_with_flags(
            directory.0.join(PRE_V2_BACKUP_FILE),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let value: String = backup
            .query_row(
                "SELECT record_value FROM explicit_records WHERE event_id = 'wal-pref'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(value, "discord");
        assert_eq!(read_migration_history(&backup).unwrap().len(), 1);
    }

    #[test]
    fn failed_migration_rolls_back_all_ddl_and_history() {
        let directory = TestDirectory::new();
        let mut connection = Connection::open(directory.database_path()).unwrap();
        let broken = Migration {
            version: 1,
            name: "injected_failure",
            sql: "CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY);\
                  CREATE TABLE partial_table (value TEXT);\
                  THIS IS NOT VALID SQL;",
        };

        assert!(run_migration(&mut connection, &broken).is_err());
        assert!(!table_exists(&connection, "schema_migrations").unwrap());
        assert!(!table_exists(&connection, "partial_table").unwrap());
        drop(connection);

        let reopened = Connection::open(directory.database_path()).unwrap();
        assert!(!has_user_tables(&reopened).unwrap());
    }

    #[test]
    fn migration_checksum_drift_denies_open() {
        let directory = TestDirectory::new();
        {
            let store = test_store(&directory);
            store
                .connection
                .execute(
                    "UPDATE schema_migrations SET checksum = ?1 WHERE version = 1",
                    ["0".repeat(64)],
                )
                .unwrap();
        }
        let mut connection = Connection::open(directory.database_path()).unwrap();
        assert_eq!(
            apply_migrations(&mut connection, MIGRATIONS),
            Err(StorageError::MigrationChecksumMismatch(1))
        );
    }

    #[test]
    fn newer_schema_version_denies_open() {
        let directory = TestDirectory::new();
        {
            let store = test_store(&directory);
            store
                .connection
                .execute(
                    "INSERT INTO schema_migrations(version, name, checksum, applied_at_ms) \
                     VALUES (3, 'future', ?1, 1)",
                    ["0".repeat(64)],
                )
                .unwrap();
        }
        let mut connection = Connection::open(directory.database_path()).unwrap();
        assert_eq!(
            apply_migrations(&mut connection, MIGRATIONS),
            Err(StorageError::NewerSchemaVersion(3))
        );
    }

    #[test]
    fn typed_preference_and_correction_roundtrip_and_latest_value() {
        let directory = TestDirectory::new();
        let mut store = test_store(&directory);
        let browser = preference_key("preferred_browser");
        store
            .record_preference(&event_id("event-1"), &browser, &reference_id("chrome"))
            .unwrap();
        store
            .record_preference(&event_id("event-2"), &browser, &reference_id("firefox"))
            .unwrap();
        store
            .record_correction(
                &event_id("event-3"),
                &correction_term("counter"),
                &canonical_id("cs2"),
            )
            .unwrap();

        assert_eq!(
            store.preference(&browser).unwrap().unwrap().target,
            reference_id("firefox")
        );
        assert_eq!(
            store
                .correction(&correction_term("counter"))
                .unwrap()
                .unwrap()
                .canonical_id,
            canonical_id("cs2")
        );
        assert!(store
            .preference(&preference_key("missing"))
            .unwrap()
            .is_none());
    }

    #[test]
    fn event_ids_are_idempotent_and_conflicting_reuse_is_denied() {
        let directory = TestDirectory::new();
        let mut store = test_store(&directory);
        let event = event_id("request-42");
        let key = preference_key("preferred_browser");
        let value = reference_id("chrome");

        assert_eq!(
            store.record_preference(&event, &key, &value).unwrap(),
            RecordOutcome::Inserted
        );
        assert_eq!(
            store.record_preference(&event, &key, &value).unwrap(),
            RecordOutcome::AlreadyRecorded
        );
        assert_eq!(
            store.record_preference(&event, &key, &reference_id("firefox")),
            Err(StorageError::IdempotencyConflict)
        );
        assert_eq!(
            store.record_correction(
                &event,
                &correction_term("preferred_browser"),
                &canonical_id("chrome"),
            ),
            Err(StorageError::IdempotencyConflict)
        );
    }

    #[test]
    fn forgetting_is_revision_bound_and_keeps_an_audit_tombstone() {
        let directory = TestDirectory::new();
        let key = preference_key("preferred_browser");
        let mut store = test_store(&directory);
        store
            .record_preference(&event_id("save-1"), &key, &reference_id("chrome"))
            .unwrap();
        store
            .record_preference(&event_id("save-2"), &key, &reference_id("firefox"))
            .unwrap();

        assert_eq!(
            store.forget_preference(&event_id("forget-1"), &key, &event_id("save-1")),
            Err(StorageError::StalePreferenceRevision)
        );
        assert_eq!(
            store
                .forget_preference(&event_id("forget-1"), &key, &event_id("save-2"))
                .unwrap(),
            RecordOutcome::Inserted
        );
        assert_eq!(
            store
                .forget_preference(&event_id("forget-1"), &key, &event_id("save-2"))
                .unwrap(),
            RecordOutcome::AlreadyRecorded
        );
        assert!(store.preference(&key).unwrap().is_none());
        let tombstone = store.preference_tombstone(&key).unwrap().unwrap();
        assert_eq!(tombstone.event_id, event_id("forget-1"));
        assert_eq!(tombstone.superseded_event_id, event_id("save-2"));
        let original_count: i64 = store
            .connection
            .query_row(
                "SELECT COUNT(*) FROM explicit_records WHERE record_key = ?1",
                [key.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(original_count, 3);
        drop(store);

        let reopened = test_store(&directory);
        assert!(reopened.preference(&key).unwrap().is_none());
        assert_eq!(
            reopened
                .preference_tombstone(&key)
                .unwrap()
                .unwrap()
                .event_id,
            event_id("forget-1")
        );
    }

    #[test]
    fn forgotten_key_rejects_delayed_saves_even_after_restore() {
        let directory = TestDirectory::new();
        let mut store = test_store(&directory);
        let key = preference_key("preferred_browser");
        let original = event_id("save-original");
        store
            .record_preference(&original, &key, &reference_id("chrome"))
            .unwrap();
        store
            .forget_preference(&event_id("forget-1"), &key, &original)
            .unwrap();

        // A retry of the original event is harmless, while a first delivery
        // of any old save is denied without relying on event-ID chronology.
        assert_eq!(
            store
                .record_preference(&original, &key, &reference_id("chrome"))
                .unwrap(),
            RecordOutcome::AlreadyRecorded
        );
        assert_eq!(
            store.record_preference(&event_id("delayed-old-save"), &key, &reference_id("chrome")),
            Err(StorageError::PreferenceForgotten)
        );
        assert!(store.preference(&key).unwrap().is_none());

        assert_eq!(
            store.restore_preference(
                &event_id("restore-1"),
                &key,
                &reference_id("firefox"),
                &event_id("wrong-tombstone"),
            ),
            Err(StorageError::StalePreferenceRevision)
        );
        assert_eq!(
            store
                .restore_preference(
                    &event_id("restore-1"),
                    &key,
                    &reference_id("firefox"),
                    &event_id("forget-1"),
                )
                .unwrap(),
            RecordOutcome::Inserted
        );
        assert_eq!(
            store
                .restore_preference(
                    &event_id("restore-1"),
                    &key,
                    &reference_id("firefox"),
                    &event_id("forget-1"),
                )
                .unwrap(),
            RecordOutcome::AlreadyRecorded
        );
        assert_eq!(
            store.preference(&key).unwrap().unwrap().target,
            reference_id("firefox")
        );
        assert!(store.preference_tombstone(&key).unwrap().is_none());
        assert_eq!(
            store.record_preference(&event_id("delayed-again"), &key, &reference_id("chrome")),
            Err(StorageError::PreferenceForgotten)
        );
        assert_eq!(
            store.preference(&key).unwrap().unwrap().target,
            reference_id("firefox")
        );
    }

    #[test]
    fn stale_forget_and_restore_cannot_cross_newer_revision() {
        let directory = TestDirectory::new();
        let mut store = test_store(&directory);
        let key = preference_key("preferred_browser");
        store
            .record_preference(&event_id("save-1"), &key, &reference_id("chrome"))
            .unwrap();
        store
            .forget_preference(&event_id("forget-1"), &key, &event_id("save-1"))
            .unwrap();
        store
            .restore_preference(
                &event_id("restore-1"),
                &key,
                &reference_id("firefox"),
                &event_id("forget-1"),
            )
            .unwrap();
        assert_eq!(
            store.forget_preference(&event_id("stale-forget"), &key, &event_id("save-1")),
            Err(StorageError::StalePreferenceRevision)
        );
        store
            .forget_preference(&event_id("forget-2"), &key, &event_id("restore-1"))
            .unwrap();
        assert_eq!(
            store.restore_preference(
                &event_id("stale-restore"),
                &key,
                &reference_id("chrome"),
                &event_id("forget-1"),
            ),
            Err(StorageError::StalePreferenceRevision)
        );
        assert_eq!(
            store.forget_preference(&event_id("forget-2"), &key, &event_id("save-1")),
            Err(StorageError::IdempotencyConflict)
        );
        assert!(store.preference(&key).unwrap().is_none());
    }

    #[test]
    fn identifiers_enforce_nonempty_length_and_token_grammar() {
        assert_eq!(EventId::new(""), Err(StorageError::InvalidIdentifier));
        assert_eq!(
            PreferenceKey::new("contains whitespace"),
            Err(StorageError::InvalidIdentifier)
        );
        assert_eq!(
            ReferenceId::new("line\nbreak"),
            Err(StorageError::InvalidIdentifier)
        );
        assert_eq!(
            CorrectionTerm::new("x".repeat(MAX_IDENTIFIER_BYTES + 1)),
            Err(StorageError::InvalidIdentifier)
        );
        assert!(CanonicalId::new("app:chrome_v2-1").is_ok());
    }

    #[test]
    fn capacity_rejection_leaves_existing_state_unchanged() {
        let directory = TestDirectory::new();
        let mut store = test_store(&directory);
        {
            let tx = store
                .connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            for sequence in 1..=MAX_STORED_RECORDS {
                tx.execute(
                    "INSERT INTO explicit_records \
                     (sequence, event_id, record_kind, record_key, record_value, created_at_ms) \
                     VALUES (?1, ?2, 'preference', 'filled_key', 'filled_value', 1)",
                    params![sequence, format!("filled-{sequence}")],
                )
                .unwrap();
            }
            tx.commit().unwrap();
        }

        assert_eq!(
            store.record_preference(
                &event_id("one-too-many"),
                &preference_key("new_key"),
                &reference_id("new_value"),
            ),
            Err(StorageError::CapacityReached)
        );
        let count: i64 = store
            .connection
            .query_row("SELECT COUNT(*) FROM explicit_records", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, MAX_STORED_RECORDS);
        assert!(store
            .preference(&preference_key("new_key"))
            .unwrap()
            .is_none());
        assert_eq!(
            store
                .preference(&preference_key("filled_key"))
                .unwrap()
                .unwrap()
                .target,
            reference_id("filled_value")
        );
    }

    #[test]
    fn production_open_cannot_be_given_an_arbitrary_path() {
        fn accepts_no_path(_: fn() -> Result<Store, StorageError>) {}
        accepts_no_path(Store::open_local_app_data);
    }

    #[cfg(feature = "test-util")]
    #[test]
    fn test_util_opens_only_under_the_given_test_root() {
        let directory = TestDirectory::new();
        let store = Store::open_in_test_root(&directory.0).unwrap();
        assert!(directory.database_path().exists());
        drop(store);
    }
}
