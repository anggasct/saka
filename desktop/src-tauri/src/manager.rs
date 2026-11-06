use std::sync::{Arc, Mutex};

use serde::Serialize;
use uuid::Uuid;

use crate::address::ServerAddress;
use crate::error::ShellError;
use crate::pairing;
use crate::secrets::{SecretStore, Token};
use crate::sidecar::{self, Phase, Sidecar};
use crate::store::{Book, Saved, Store};

const NAME_MAX: usize = 100;
pub const LOCAL_ID: &str = "local";
const LOCAL_NAME: &str = "This computer";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Remote,
    Local,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct View {
    pub id: String,
    pub name: String,
    pub url: String,
    pub active: bool,
    pub kind: Kind,
    pub running: bool,
}

#[derive(Serialize)]
pub struct Active {
    pub id: String,
    pub url: String,
    pub token: Option<String>,
    pub kind: Kind,
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
    sidecar: Sidecar,
}

fn local_url(port: u16) -> String {
    format!("http://127.0.0.1:{port}")
}

impl Manager {
    pub fn new(
        store: Store,
        secrets: Arc<dyn SecretStore>,
        http: reqwest::Client,
        sidecar: Sidecar,
    ) -> Manager {
        Manager {
            store: Mutex::new(store),
            secrets,
            http,
            sidecar,
        }
    }

    pub fn sidecar(&self) -> &Sidecar {
        &self.sidecar
    }

    pub fn http(&self) -> &reqwest::Client {
        &self.http
    }

    pub fn prepare_local_port(&self) -> Result<u16, ShellError> {
        let store = self.book()?;
        let mut book = store.load();
        if let Some(port) = book.local_port
            && (self.sidecar.phase() == Phase::Running || sidecar::port_is_free(port))
        {
            return Ok(port);
        }
        let port = sidecar::free_port().ok_or(ShellError::LocalStartFailed)?;
        book.local_port = Some(port);
        store.save(&book)?;
        Ok(port)
    }

    pub async fn local_start(&self) -> Result<(), ShellError> {
        let port = self.prepare_local_port()?;
        self.sidecar.start(port).await
    }

    pub async fn local_stop(&self) {
        self.sidecar.stop().await;
    }

    fn local_view(&self, book: &Book) -> View {
        View {
            id: LOCAL_ID.to_string(),
            name: LOCAL_NAME.to_string(),
            url: book.local_port.map(local_url).unwrap_or_default(),
            active: book.active.as_deref() == Some(LOCAL_ID),
            kind: Kind::Local,
            running: self.sidecar.phase() == Phase::Running,
        }
    }

    fn book(&self) -> Result<std::sync::MutexGuard<'_, Store>, ShellError> {
        self.store.lock().map_err(|_| ShellError::StoreFailed)
    }

    pub fn active_origin(&self) -> Option<String> {
        let book = self.book().ok()?.load();
        let id = book.active.clone()?;
        if id == LOCAL_ID {
            return book.local_port.map(local_url);
        }
        book.connections
            .into_iter()
            .find(|c| c.id == id)
            .map(|c| c.url)
    }

    pub fn local_is_active(&self) -> bool {
        self.book()
            .is_ok_and(|store| store.load().active.as_deref() == Some(LOCAL_ID))
    }

    pub fn active_target(&self) -> Option<(String, Option<String>)> {
        let book = self.book().ok()?.load();
        let id = book.active.clone()?;
        if id == LOCAL_ID {
            return book.local_port.map(|port| (local_url(port), None));
        }
        let saved = book.connections.into_iter().find(|c| c.id == id)?;
        let token = self.secrets.load(&saved.id).ok().flatten();
        Some((saved.url, token.map(|t| t.expose().to_string())))
    }

    pub fn list(&self) -> Result<Listing, ShellError> {
        let book = self.book()?.load();
        let mut views = vec![self.local_view(&book)];
        views.extend(book.connections.iter().map(|c| View {
            id: c.id.clone(),
            name: c.name.clone(),
            url: c.url.clone(),
            active: book.active.as_deref() == Some(c.id.as_str()),
            kind: Kind::Remote,
            running: false,
        }));
        let active = if book.active.as_deref() == Some(LOCAL_ID) {
            Some(Active {
                id: LOCAL_ID.to_string(),
                url: book.local_port.map(local_url).unwrap_or_default(),
                token: None,
                kind: Kind::Local,
            })
        } else {
            book.connections
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
                    kind: Kind::Remote,
                })
        };
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
            kind: Kind::Remote,
            running: false,
        })
    }

    pub fn remove(&self, id: &str) -> Result<(), ShellError> {
        if id == LOCAL_ID {
            return Err(ShellError::NotFound);
        }
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
        if id != LOCAL_ID && !book.connections.iter().any(|c| c.id == id) {
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
        let sidecar = Sidecar::new(
            dir.path().join("saka"),
            dir.path().join("local"),
            pairing::client().unwrap(),
            crate::sidecar::Timing::default(),
        );
        let manager = Manager::new(
            Store::new(file.clone()),
            secrets.clone(),
            pairing::client().unwrap(),
            sidecar,
        );
        Rig {
            manager,
            secrets,
            file,
            _dir: dir,
        }
    }

    fn remote(manager: &Manager) -> Vec<View> {
        manager
            .list()
            .unwrap()
            .connections
            .into_iter()
            .filter(|c| c.kind == Kind::Remote)
            .collect()
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
        let listing = remote(&rig.manager);
        assert_eq!(listing.len(), 1);
        assert_eq!(listing[0].name, "Office 2");
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
        assert!(remote(&rig.manager).is_empty());
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
        assert!(remote(&rig.manager).is_empty());
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
        assert_eq!(rig.manager.active_origin().as_deref(), Some(b.url.as_str()));
        rig.manager.remove(&b.id).unwrap();
        assert_eq!(rig.secrets.ids(), vec![a.id.clone()]);
        assert_eq!(remote(&rig.manager).len(), 1);
        assert!(rig.manager.list().unwrap().active.is_none());
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

    #[test]
    fn this_computer_is_always_listed_first_and_can_be_selected_but_not_removed() {
        let rig = rig();
        let port = rig.manager.prepare_local_port().unwrap();
        let listing = rig.manager.list().unwrap();
        let local = &listing.connections[0];
        assert_eq!((local.id.as_str(), local.kind), (LOCAL_ID, Kind::Local));
        assert_eq!(local.url, format!("http://127.0.0.1:{port}"));
        assert!(!local.active && !local.running);
        rig.manager.select(LOCAL_ID).unwrap();
        let listing = rig.manager.list().unwrap();
        assert!(listing.connections[0].active);
        let active = listing.active.unwrap();
        assert_eq!((active.kind, active.token), (Kind::Local, None));
        assert_eq!(
            rig.manager.active_origin(),
            Some(format!("http://127.0.0.1:{port}"))
        );
        assert_eq!(
            rig.manager.active_target(),
            Some((format!("http://127.0.0.1:{port}"), None))
        );
        assert_eq!(rig.manager.remove(LOCAL_ID), Err(ShellError::NotFound));
    }

    #[test]
    fn the_local_port_is_kept_while_free_and_replaced_when_something_else_took_it() {
        let rig = rig();
        let first = rig.manager.prepare_local_port().unwrap();
        assert_eq!(rig.manager.prepare_local_port().unwrap(), first);
        let taken = std::net::TcpListener::bind(("127.0.0.1", first)).unwrap();
        let second = rig.manager.prepare_local_port().unwrap();
        assert_ne!(second, first);
        drop(taken);
        assert_eq!(rig.manager.prepare_local_port().unwrap(), second);
    }

    #[tokio::test]
    async fn starting_the_local_saka_without_its_program_is_an_error_not_a_panic() {
        let rig = rig();
        assert_eq!(
            rig.manager.local_start().await,
            Err(ShellError::SidecarMissing)
        );
    }
}
