pub mod authorship;
mod codec;
mod conn;
pub mod draft;
mod error;
mod jobs;
pub mod list;
mod migrate;
pub mod read;
pub mod user_link;
mod write;

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rusqlite::{Connection, OpenFlags, TransactionBehavior};
use saka_core::diagnostics::MigrationState;
use saka_core::time::Timestamp;
use uuid::Uuid;

pub use draft::DraftSet;
pub use error::{Result, StoreError};
pub use jobs::{ClaimedJob, LEASE_EXTEND_MS, LEASE_MS};
pub use migrate::MigrationReport;
pub use read::{StoredEvent, TaskSet};
pub use user_link::{AuthorizationRow, SealedColumns, UserLinkRow};
pub use write::{
    ActionRecord, Batch, ContextBlobRow, EventRecord, JobOutcome, NewJob, SecretRow,
    SpecRevisionRow, WebhookDeliveryRow,
};

pub struct Store {
    path: PathBuf,
    writer: Mutex<Connection>,
    readers: conn::Readers,
    migration: MigrationReport,
}

impl Store {
    pub fn open(path: &Path, backups_dir: &Path, now: Timestamp) -> Result<Store> {
        let mut writer = conn::open_writer(path)?;
        let migration = migrate::migrate(&mut writer, backups_dir, now)?;
        Ok(Store {
            path: path.to_path_buf(),
            writer: Mutex::new(writer),
            readers: conn::Readers::new(path),
            migration,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn migration(&self) -> &MigrationReport {
        &self.migration
    }

    pub fn commit(&self, batch: &Batch) -> Result<Vec<i64>> {
        let mut conn = self.writer.lock().map_err(|_| StoreError::Poisoned)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let seqs = write::write(&tx, batch)?;
        tx.commit()?;
        Ok(seqs)
    }

    pub fn read<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        self.readers.with(f)
    }

    pub fn claim_job(&self, owner: &str, now: Timestamp) -> Result<Option<ClaimedJob>> {
        let conn = self.writer.lock().map_err(|_| StoreError::Poisoned)?;
        jobs::claim(&conn, owner, now)
    }

    pub fn extend_lease(&self, job: Uuid, owner: &str, now: Timestamp) -> Result<Timestamp> {
        let conn = self.writer.lock().map_err(|_| StoreError::Poisoned)?;
        jobs::extend_lease(&conn, job, owner, now)
    }

    pub fn job_counts(&self) -> Result<Vec<(String, String, i64)>> {
        self.read(jobs::counts)
    }

    pub fn oldest_ready_job_age_ms(&self, now: Timestamp) -> Result<Option<i64>> {
        self.read(|c| jobs::oldest_ready_age(c, now))
    }

    pub fn schema_version(&self) -> Result<Option<i64>> {
        self.read(migrate::current_version)
    }
}

pub fn latest_migration() -> i64 {
    migrate::MIGRATIONS.iter().map(|m| m.0).max().unwrap_or(0)
}

pub fn migration_status(path: &Path) -> Result<MigrationState> {
    let latest = latest_migration();
    if !path.exists() {
        return Ok(MigrationState { applied: 0, latest });
    }
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    let applied = migrate::current_version(&conn)?.unwrap_or(0);
    Ok(MigrationState { applied, latest })
}
