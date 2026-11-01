//! Store errors. Driver errors are mapped once here; callers never see SQLite codes.

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("migration {version} was changed after it was applied")]
    MigrationChecksumMismatch { version: i64 },
    #[error("database is at migration {database}, this binary knows up to {binary}")]
    MigrationVersionAhead { database: i64, binary: i64 },
    #[error("job {job} is no longer leased by this worker")]
    LeaseLost { job: String },
    #[error("a row violates a constraint: {0}")]
    Constraint(String),
    #[error("database is busy")]
    Busy,
    #[error("stored value is not valid: {0}")]
    Corrupt(String),
    #[error("connection setting {setting} is {actual}, expected {expected}")]
    Setting {
        setting: &'static str,
        expected: String,
        actual: String,
    },
    #[error("database lock poisoned")]
    Poisoned,
    #[error("file error: {0}")]
    Io(#[from] std::io::Error),
    #[error("database error: {0}")]
    Sqlite(rusqlite::Error),
}

impl StoreError {
    /// Stable code for logs and process exit messages.
    pub fn code(&self) -> &'static str {
        match self {
            StoreError::MigrationChecksumMismatch { .. } => "migration_checksum_mismatch",
            StoreError::MigrationVersionAhead { .. } => "migration_version_ahead",
            StoreError::LeaseLost { .. } => "lease_lost",
            StoreError::Constraint(_) => "constraint_violation",
            StoreError::Busy => "database_busy",
            StoreError::Corrupt(_) => "corrupt_value",
            StoreError::Setting { .. } => "connection_setting",
            StoreError::Poisoned => "lock_poisoned",
            StoreError::Io(_) => "io_error",
            StoreError::Sqlite(_) => "database_error",
        }
    }
}

impl From<rusqlite::Error> for StoreError {
    fn from(err: rusqlite::Error) -> StoreError {
        use rusqlite::ErrorCode;
        match err.sqlite_error_code() {
            Some(ErrorCode::ConstraintViolation) => StoreError::Constraint(err.to_string()),
            Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked) => StoreError::Busy,
            _ => StoreError::Sqlite(err),
        }
    }
}

impl From<serde_json::Error> for StoreError {
    fn from(err: serde_json::Error) -> StoreError {
        StoreError::Corrupt(err.to_string())
    }
}

pub type Result<T> = std::result::Result<T, StoreError>;
