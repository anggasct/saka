use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::ShellError;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Saved {
    pub id: String,
    pub name: String,
    pub url: String,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Book {
    pub active: Option<String>,
    pub connections: Vec<Saved>,
    pub local_port: Option<u16>,
}

pub struct Store {
    path: PathBuf,
}

impl Store {
    pub fn new(path: PathBuf) -> Store {
        Store { path }
    }

    pub fn load(&self) -> Book {
        match std::fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                tracing::warn!(error = %e, "connection list unreadable, starting empty");
                Book::default()
            }),
            Err(_) => Book::default(),
        }
    }

    pub fn save(&self, book: &Book) -> Result<(), ShellError> {
        let write = || -> std::io::Result<()> {
            if let Some(parent) = self.path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let temp = self.path.with_extension("json.tmp");
            std::fs::write(&temp, serde_json::to_vec_pretty(book)?)?;
            std::fs::rename(&temp, &self.path)
        };
        write().map_err(|e| {
            tracing::warn!(error = %e, "connection list not saved");
            ShellError::StoreFailed
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn book() -> Book {
        Book {
            active: Some("a".into()),
            connections: vec![Saved {
                id: "a".into(),
                name: "Office".into(),
                url: "https://saka.example.com".into(),
            }],
            local_port: Some(40123),
        }
    }

    #[test]
    fn a_saved_list_round_trips_through_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path().join("nested").join("connections.json"));
        assert_eq!(store.load(), Book::default());
        store.save(&book()).unwrap();
        assert_eq!(store.load(), book());
    }

    #[test]
    fn a_damaged_file_reads_as_an_empty_list() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connections.json");
        std::fs::write(&path, b"{not json").unwrap();
        assert_eq!(Store::new(path).load(), Book::default());
    }
}
