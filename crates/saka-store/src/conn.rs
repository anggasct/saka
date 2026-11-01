use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use rusqlite::{Connection, OpenFlags};

use crate::error::{Result, StoreError};

const BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const READERS: usize = 4;

pub(crate) fn open_writer(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path)?;
    // busy_timeout before journal_mode, so switching to WAL waits instead of failing.
    conn.busy_timeout(BUSY_TIMEOUT)?;
    let mode: String = conn.query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))?;
    expect("journal_mode", "wal", &mode.to_lowercase())?;
    configure(&conn)?;
    Ok(conn)
}

fn open_reader(path: &Path) -> Result<Connection> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.busy_timeout(BUSY_TIMEOUT)?;
    configure(&conn)?;
    conn.pragma_update(None, "query_only", "ON")?;
    Ok(conn)
}

fn configure(conn: &Connection) -> Result<()> {
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    let fk: i64 = conn.query_row("PRAGMA foreign_keys", [], |r| r.get(0))?;
    expect("foreign_keys", "1", &fk.to_string())?;
    let sync: i64 = conn.query_row("PRAGMA synchronous", [], |r| r.get(0))?;
    expect("synchronous", "1", &sync.to_string())
}

fn expect(setting: &'static str, expected: &str, actual: &str) -> Result<()> {
    if actual == expected {
        Ok(())
    } else {
        Err(StoreError::Setting {
            setting,
            expected: expected.to_string(),
            actual: actual.to_string(),
        })
    }
}

pub(crate) struct Readers {
    path: PathBuf,
    idle: Mutex<Vec<Connection>>,
}

impl Readers {
    pub(crate) fn new(path: &Path) -> Readers {
        Readers {
            path: path.to_path_buf(),
            idle: Mutex::new(Vec::new()),
        }
    }

    pub(crate) fn with<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let conn = match self.idle.lock().map_err(|_| StoreError::Poisoned)?.pop() {
            Some(conn) => conn,
            None => open_reader(&self.path)?,
        };
        let result = f(&conn);
        let mut idle = self.idle.lock().map_err(|_| StoreError::Poisoned)?;
        if idle.len() < READERS {
            idle.push(conn);
        }
        result
    }
}
