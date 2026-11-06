use std::sync::{Arc, Mutex};

use serde::Serialize;
use uuid::Uuid;

use crate::address::ServerAddress;
use crate::error::ShellError;
use crate::pairing;
use crate::secrets::{SecretStore, Token};
use crate::store::{Book, Saved, Store};

const NAME_MAX: usize = 100;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct View {
    pub id: String,
    pub name: String,
    pub url: String,
    pub active: bool,
}

#[derive(Serialize)]
pub struct Active {
    pub id: String,
    pub url: String,
    pub token: Option<String>,
}

#[derive(Serialize)]
pub struct Listing {
    pub connections: Vec<View>,
    pub active: Option<Active>,
}

pub struct Manager {
    store: Mutex<Store>,
    secrets: Arc<dyn SecretStore>,
    http: reqwest::Client,
}

impl Manager {
    pub fn new(store: Store, secrets: Arc<dyn SecretStore>, http: reqwest::Client) -> Manager {
        Manager {
            store: Mutex::new(store),
            secrets,
            http,
        }
    }

    fn book(&self) -> Result<std::sync::MutexGuard<'_, Store>, ShellError> {
        self.store.lock().map_err(|_| ShellError::StoreFailed)
    }

    pub fn active_url(&self) -> Option<String> {
        let book = self.book().ok()?.load();
        let id = book.active?;
        book.connections
            .into_iter()
            .find(|c| c.id == id)
            .map(|c| c.url)
    }

    pub fn list(&self) -> Result<Listing, ShellError> {
        let book = self.book()?.load();
        let views = book
            .connections
            .iter()
            .map(|c| View {
                id: c.id.clone(),
                name: c.name.clone(),
                url: c.url.clone(),
                active: book.active.as_deref() == Some(c.id.as_str()),
            })
            .collect();
        let active = book
            .connections
            .iter()
            .find(|c| book.active.as_deref() == Some(c.id.as_str()))
            .map(|c| Active {
                id: c.id.clone(),
                url: c.url.clone(),
                token: self
                    .secrets
                    .load(&c.id)
                    .ok()
                    .flatten()
                    .map(|t| t.expose().to_string()),
            });
        Ok(Listing {
            connections: views,
            active,
        })
    }

    pub async fn add(&self, name: &str, url: &str, code: &str) -> Result<View, ShellError> {
        let name = name.trim();
        if name.is_empty() || name.chars().count() > NAME_MAX {
            return Err(ShellError::InvalidName);
        }
        let address = ServerAddress::parse(url)?;
        let token = pairing::pair(&self.http, &address, code).await?;
        self.keep(name, &address, &token)
    }

    fn keep(&self, name: &str, address: &ServerAddress, token: &Token) -> Result<View, ShellError> {
        let store = self.book()?;
        let mut book = store.load();
        let existing = book
            .connections
            .iter()
            .position(|c| c.url == address.as_str());
        let id = existing.map_or_else(
            || Uuid::new_v4().to_string(),
            |i| book.connections[i].id.clone(),
        );
        self.secrets.save(&id, token)?;
        let saved = Saved {
            id: id.clone(),
            name: name.to_string(),
            url: address.as_str().to_string(),
        };
        match existing {
            Some(i) => book.connections[i] = saved.clone(),
            None => book.connections.push(saved.clone()),
        }
        if book.active.is_none() {
            book.active = Some(id.clone());
        }
        if let Err(e) = store.save(&book) {
            if existing.is_none() {
                let _ = self.secrets.remove(&id);
            }
            return Err(e);
        }
        Ok(View {
            active: book.active.as_deref() == Some(id.as_str()),
            id,
            name: saved.name,
            url: saved.url,
        })
    }

    pub fn remove(&self, id: &str) -> Result<(), ShellError> {
        let store = self.book()?;
        let mut book = store.load();
        if !book.connections.iter().any(|c| c.id == id) {
            return Err(ShellError::NotFound);
        }
        self.secrets.remove(id)?;
        book.connections.retain(|c| c.id != id);
        if book.active.as_deref() == Some(id) {
            book.active = None;
        }
        store.save(&book)
    }

    pub fn select(&self, id: &str) -> Result<(), ShellError> {
        let store = self.book()?;
        let mut book: Book = store.load();
        if !book.connections.iter().any(|c| c.id == id) {
            return Err(ShellError::NotFound);
        }
        book.active = Some(id.to_string());
        store.save(&book)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;
    use crate::secrets::memory::Memory;

    struct Rig {
        manager: Manager,
        secrets: Arc<Memory>,
        file: std::path::PathBuf,
        _dir: tempfile::TempDir,
    }

    fn rig() -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("connections.json");
        let secrets = Arc::new(Memory::default());
        let manager = Manager::new(
            Store::new(file.clone()),
            secrets.clone(),
            pairing::client().unwrap(),
        );
        Rig {
            manager,
            secrets,
            file,
            _dir: dir,
        }
    }

    async fn server(token: &str) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/pair"))
            .respond_with(
                ResponseTemplate::new(201)
                    .set_body_json(json!({ "device_id": "d", "token": token })),
            )
            .mount(&server)
            .await;
        server
    }

    #[tokio::test]
    async fn adding_pairs_keeps_the_token_in_the_keychain_and_never_in_the_file() {
        let rig = rig();
        let server = server("sdt_topsecret").await;
        let view = rig
            .manager
            .add("Office", &server.uri(), "WDJB-MJHT")
            .await
            .unwrap();
        assert_eq!(view.name, "Office");
        assert!(view.active);
        assert_eq!(
            rig.secrets.value(&view.id).as_deref(),
            Some("sdt_topsecret")
        );
        let written = std::fs::read_to_string(&rig.file).unwrap();
        assert!(!written.contains("sdt_topsecret"), "{written}");
        assert!(written.contains("Office"));
        let listing = rig.manager.list().unwrap();
        let active = listing.active.unwrap();
        assert_eq!(active.token.as_deref(), Some("sdt_topsecret"));
        assert_eq!(active.url, view.url);
    }

    #[tokio::test]
    async fn pairing_again_to_the_same_server_replaces_the_token_and_keeps_the_entry() {
        let rig = rig();
        let first = server("sdt_one").await;
        let view = rig.manager.add("Office", &first.uri(), "A").await.unwrap();
        first.reset().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/pair"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "token": "sdt_two" })))
            .mount(&first)
            .await;
        let again = rig
            .manager
            .add("Office 2", &first.uri(), "B")
            .await
            .unwrap();
        assert_eq!(again.id, view.id);
        assert_eq!(rig.secrets.value(&view.id).as_deref(), Some("sdt_two"));
        let listing = rig.manager.list().unwrap();
        assert_eq!(listing.connections.len(), 1);
        assert_eq!(listing.connections[0].name, "Office 2");
    }

    #[tokio::test]
    async fn a_refused_code_or_a_bad_address_changes_nothing() {
        let rig = rig();
        let refusing = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(400).set_body_json(json!({ "code": "pairing_code_invalid" })),
            )
            .mount(&refusing)
            .await;
        assert_eq!(
            rig.manager
                .add("x", &refusing.uri(), "bad")
                .await
                .unwrap_err(),
            ShellError::PairingCodeInvalid
        );
        for (name, url, expected) in [
            ("x", "http://saka.example.com", ShellError::InsecureUrl),
            ("x", "nonsense", ShellError::InvalidUrl),
            ("  ", "https://saka.example.com", ShellError::InvalidName),
        ] {
            assert_eq!(rig.manager.add(name, url, "c").await.unwrap_err(), expected);
        }
        assert!(rig.manager.list().unwrap().connections.is_empty());
        assert!(rig.secrets.ids().is_empty());
        assert!(!rig.file.exists());
    }

    #[tokio::test]
    async fn an_unavailable_keychain_fails_the_add_without_saving_the_connection() {
        let rig = rig();
        *rig.secrets.broken.lock().unwrap() = true;
        let server = server("sdt_x").await;
        let error = rig
            .manager
            .add("Office", &server.uri(), "c")
            .await
            .unwrap_err();
        assert_eq!(error, ShellError::KeychainUnavailable);
        assert!(rig.manager.list().unwrap().connections.is_empty());
    }

    #[tokio::test]
    async fn selecting_switches_the_active_connection_and_removing_deletes_its_token() {
        let rig = rig();
        let one = server("sdt_one").await;
        let two = server("sdt_two").await;
        let a = rig.manager.add("A", &one.uri(), "c").await.unwrap();
        let b = rig.manager.add("B", &two.uri(), "c").await.unwrap();
        assert!(a.active && !b.active);
        rig.manager.select(&b.id).unwrap();
        assert_eq!(rig.manager.active_url().as_deref(), Some(b.url.as_str()));
        rig.manager.remove(&b.id).unwrap();
        assert_eq!(rig.secrets.ids(), vec![a.id.clone()]);
        let listing = rig.manager.list().unwrap();
        assert_eq!(listing.connections.len(), 1);
        assert!(listing.active.is_none());
        assert_eq!(rig.manager.remove(&b.id), Err(ShellError::NotFound));
        assert_eq!(rig.manager.select("nope"), Err(ShellError::NotFound));
    }

    #[tokio::test]
    async fn a_token_missing_from_the_keychain_lists_the_active_connection_without_one() {
        let rig = rig();
        let server = server("sdt_x").await;
        let view = rig.manager.add("Office", &server.uri(), "c").await.unwrap();
        rig.secrets.remove(&view.id).unwrap();
        let active = rig.manager.list().unwrap().active.unwrap();
        assert_eq!(active.token, None);
    }
}
