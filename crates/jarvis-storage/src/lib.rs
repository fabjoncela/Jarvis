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
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OpenFlags, OptionalExtension, TransactionBehavior};
use sha2::{Digest, Sha256};

const DATABASE_FILE: &str = "jarvis.db";
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
    IdempotencyConflict,
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
            Self::IdempotencyConflict => {
                formatter.write_str("event ID was already used for a different explicit record")
            }
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

const MIGRATIONS: &[Migration] = &[Migration {
    version: 1,
    name: "explicit_records",
    sql: include_str!("../migrations/0001_explicit_records.sql"),
}];

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
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
        )?;
        connection.busy_timeout(std::time::Duration::from_millis(BUSY_TIMEOUT_MS))?;
        connection.pragma_update(None, "trusted_schema", "OFF")?;
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
        )
    }

    /// Returns the most recently appended preference for a key.
    pub fn preference(
        &self,
        key: &PreferenceKey,
    ) -> Result<Option<StoredPreference>, StorageError> {
        let row = self
            .connection
            .query_row(
                "SELECT event_id, record_key, record_value, created_at_ms \
                 FROM explicit_records \
                 WHERE record_kind = 'preference' AND record_key = ?1 \
                 ORDER BY sequence DESC LIMIT 1",
                [key.as_str()],
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
        row.map(|(event_id, key, value, created_at_ms)| {
            Ok(StoredPreference {
                event_id: EventId::new(event_id)?,
                key: PreferenceKey::new(key)?,
                target: ReferenceId::new(value)?,
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
    ) -> Result<RecordOutcome, StorageError> {
        let created_at_ms = unix_time_millis()?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = tx
            .query_row(
                "SELECT record_kind, record_key, record_value \
                 FROM explicit_records WHERE event_id = ?1",
                [event_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?;
        if let Some((existing_kind, existing_key, existing_value)) = existing {
            if existing_kind == kind && existing_key == key && existing_value == value {
                return Ok(RecordOutcome::AlreadyRecorded);
            }
            return Err(StorageError::IdempotencyConflict);
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
             (event_id, record_kind, record_key, record_value, created_at_ms) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![event_id, kind, key, value, created_at_ms],
        )?;
        tx.commit()?;
        Ok(RecordOutcome::Inserted)
    }
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
        assert_eq!(read_migration_history(&store.connection).unwrap().len(), 1);
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
        assert_eq!(read_migration_history(&connection).unwrap().len(), 1);
        drop(connection);

        let store = test_store(&directory);
        let stored = store.preference(&key).unwrap().unwrap();
        assert_eq!(stored.event_id, event_id("event-1"));
        assert_eq!(stored.target, reference_id("chrome"));
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
                     VALUES (2, 'future', ?1, 1)",
                    ["0".repeat(64)],
                )
                .unwrap();
        }
        let mut connection = Connection::open(directory.database_path()).unwrap();
        assert_eq!(
            apply_migrations(&mut connection, MIGRATIONS),
            Err(StorageError::NewerSchemaVersion(2))
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
