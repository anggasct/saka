//! SQLite storage: migrations, the event log with projections and jobs in one transaction,
//! the job queue, and read queries.
//!
//! One writer connection serializes every write; readers use a small pool of read-only
//! connections. Nothing here decides anything: the rows come from the core.

mod codec;
mod conn;
mod error;
mod jobs;
mod migrate;
pub mod read;
mod write;

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rusqlite::{Connection, TransactionBehavior};
use saka_core::time::Timestamp;
use uuid::Uuid;

pub use error::{Result, StoreError};
pub use jobs::{ClaimedJob, LEASE_EXTEND_MS, LEASE_MS};
pub use migrate::MigrationReport;
pub use read::{StoredEvent, TaskSet};
pub use write::{ActionRecord, Batch, EventRecord, JobOutcome, NewJob, SpecRevisionRow};

pub struct Store {
    path: PathBuf,
    writer: Mutex<Connection>,
    readers: conn::Readers,
    migration: MigrationReport,
}

impl Store {
    /// Opens (creating if needed) the database at `path`, backs it up into `backups_dir`
    /// when migrations are pending on an existing database, and migrates it.
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

    /// What `open` migrated and where it put the backup.
    pub fn migration(&self) -> &MigrationReport {
        &self.migration
    }

    /// Writes the batch in one `BEGIN IMMEDIATE` transaction; returns the event seqs.
    pub fn commit(&self, batch: &Batch) -> Result<Vec<i64>> {
        let mut conn = self.writer.lock().map_err(|_| StoreError::Poisoned)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let seqs = write::write(&tx, batch)?;
        tx.commit()?;
        Ok(seqs)
    }

    /// Runs read queries on a read-only connection.
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

    /// Highest applied migration.
    pub fn schema_version(&self) -> Result<Option<i64>> {
        self.read(migrate::current_version)
    }
}
