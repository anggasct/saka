#![allow(
    dead_code,
    reason = "each test binary uses a different part of the shared GitHub harness"
)]

use std::os::unix::fs::PermissionsExt;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use async_trait::async_trait;
use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use saka_core::ids::RepoId;
use saka_core::time::Timestamp;
use saka_ports::{
    Blob, Clock, CommitOne, CommitPush, PortError, PortResult, PushCommit, PushOutcome,
    PushRequest, RepoRef, Repos, TreeEntry,
};
use saka_scm::{GitHub, GitHubApp, GitHubConfig};
use serde_json::{Value, json};
use uuid::Uuid;
use wiremock::matchers::{body_json, method, path};
use wiremock::{Match, Mock, MockServer, Request, ResponseTemplate};

pub const APP_ID: u64 = 123_456;
pub const NOW_MS: i64 = 1_767_225_600_000;
pub const HEAD: &str = "1111111111111111111111111111111111111111";
pub const MERGED: &str = "2222222222222222222222222222222222222222";

pub struct TestClock(pub AtomicI64);

impl Clock for TestClock {
    fn now(&self) -> Timestamp {
        Timestamp(self.0.load(Ordering::SeqCst))
    }
}

pub struct NoRepos;

#[async_trait]
impl Repos for NoRepos {
    async fn fetch(&self, _repo: &RepoRef) -> PortResult<()> {
        Ok(())
    }
    async fn tip(&self, _repo: &RepoRef, _branch: &str) -> PortResult<Option<String>> {
        Ok(None)
    }
    async fn read_file(&self, _r: &RepoRef, _c: &str, _p: &str) -> PortResult<Option<Vec<u8>>> {
        Ok(None)
    }
    async fn read_bounded(&self, _r: &RepoRef, _c: &str, _p: &str, _m: u64) -> PortResult<Blob> {
        Ok(Blob::Missing)
    }
    async fn grep_paths(
        &self,
        _r: &RepoRef,
        _c: &str,
        _p: &str,
        _s: &[String],
    ) -> PortResult<Vec<String>> {
        Ok(Vec::new())
    }
    async fn push(&self, request: &PushRequest) -> PortResult<PushOutcome> {
        Ok(PushOutcome::Pushed {
            sha: request.local_sha.clone(),
        })
    }
    async fn list_tree(&self, _r: &RepoRef, _c: &str, _d: &str) -> PortResult<Vec<TreeEntry>> {
        Ok(Vec::new())
    }
    async fn entries(&self, _r: &RepoRef, _c: &str, _p: &[String]) -> PortResult<Vec<TreeEntry>> {
        Ok(Vec::new())
    }
    async fn write_blob(&self, _r: &RepoRef, _b: &[u8]) -> PortResult<String> {
        Err(PortError::permanent("unused", json!({})))
    }
    async fn commit_one(&self, _request: &CommitOne) -> PortResult<String> {
        Err(PortError::permanent("unused", json!({})))
    }
    async fn push_commit(&self, _request: &PushCommit) -> PortResult<CommitPush> {
        Err(PortError::permanent("unused", json!({})))
    }
}

pub struct AppJwt {
    pub public_pem: Vec<u8>,
    pub clock: Arc<TestClock>,
}

impl Match for AppJwt {
    fn matches(&self, request: &Request) -> bool {
        let Some(token) = request
            .headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
        else {
            return false;
        };
        let Ok(header) = jsonwebtoken::decode_header(token) else {
            return false;
        };
        let Ok(key) = DecodingKey::from_rsa_pem(&self.public_pem) else {
            return false;
        };
        let mut validation = Validation::new(Algorithm::RS256);
        validation.validate_exp = false;
        validation.required_spec_claims.clear();
        let Ok(data) = jsonwebtoken::decode::<Value>(token, &key, &validation) else {
            return false;
        };
        let now = self.clock.now().0 / 1000;
        let c = &data.claims;
        header.alg == Algorithm::RS256
            && c["iss"].as_str() == Some(APP_ID.to_string().as_str())
            && c["iat"] == now - 60
            && c["exp"]
                .as_i64()
                .is_some_and(|exp| exp > now && exp <= now + 600)
    }
}

pub struct World {
    pub _dir: tempfile::TempDir,
    pub server: MockServer,
    pub clock: Arc<TestClock>,
    pub public_pem: Vec<u8>,
    pub app: Arc<GitHubApp>,
    pub gh: GitHub,
    pub repo: RepoRef,
}

pub fn openssl(args: &[&str]) {
    let out = Command::new("openssl")
        .args(args)
        .output()
        .expect("the openssl CLI is needed to make a test key");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

pub fn rfc3339(ms: i64) -> String {
    jiff::Timestamp::from_millisecond(ms).unwrap().to_string()
}

impl World {
    pub async fn new() -> World {
        let dir = tempfile::tempdir().unwrap();
        let key = dir.path().join("app.pem");
        let public = dir.path().join("app.pub.pem");
        let k = key.to_str().unwrap();
        openssl(&[
            "genpkey",
            "-algorithm",
            "RSA",
            "-pkeyopt",
            "rsa_keygen_bits:2048",
            "-out",
            k,
        ]);
        openssl(&[
            "pkey",
            "-in",
            k,
            "-pubout",
            "-out",
            public.to_str().unwrap(),
        ]);
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
        let server = MockServer::start().await;
        let clock = Arc::new(TestClock(AtomicI64::new(NOW_MS)));
        let app = Arc::new(
            GitHubApp::new(
                GitHubConfig {
                    integration_id: "github".into(),
                    api_url: server.uri(),
                    web_url: "https://github.example".into(),
                    app_id: APP_ID,
                    private_key_file: key,
                    user_agent: "saka-test".into(),
                    client_id: None,
                },
                clock.clone(),
            )
            .unwrap(),
        );
        let gh = GitHub::new(app.clone(), Arc::new(NoRepos));
        World {
            public_pem: std::fs::read(&public).unwrap(),
            _dir: dir,
            server,
            clock,
            app,
            gh,
            repo: RepoRef {
                repo_id: RepoId(Uuid::from_u128(1)),
                integration_id: "github".into(),
                full_name: "acme/app".into(),
                url: "https://github.example/acme/app".into(),
            },
        }
    }

    pub fn jwt(&self) -> AppJwt {
        AppJwt {
            public_pem: self.public_pem.clone(),
            clock: self.clock.clone(),
        }
    }

    pub async fn mount_tokens(&self) {
        Mock::given(method("GET"))
            .and(path("/repos/acme/app/installation"))
            .and(self.jwt())
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "id": 42 })))
            .mount(&self.server)
            .await;
        let clock = self.clock.clone();
        let issued = Arc::new(AtomicI64::new(0));
        Mock::given(method("POST"))
            .and(path("/app/installations/42/access_tokens"))
            .and(body_json(json!({ "repositories": ["app"] })))
            .and(self.jwt())
            .respond_with(move |_: &Request| {
                let n = issued.fetch_add(1, Ordering::SeqCst) + 1;
                ResponseTemplate::new(201).set_body_json(json!({
                    "token": format!("ghs_{n}"),
                    "expires_at": rfc3339(clock.now().0 + 3_600_000),
                }))
            })
            .mount(&self.server)
            .await;
    }

    pub async fn token_requests(&self) -> usize {
        self.server
            .received_requests()
            .await
            .unwrap_or_default()
            .iter()
            .filter(|r| r.url.path().ends_with("/access_tokens"))
            .count()
    }

    pub fn advance_to(&self, ms: i64) {
        self.clock.0.store(ms, Ordering::SeqCst);
    }
}

pub fn pr(number: u64, state: &str, merged: bool) -> Value {
    json!({
        "number": number,
        "html_url": format!("https://github.example/acme/app/pull/{number}"),
        "state": state,
        "merged": merged,
        "merged_at": if merged { json!("2026-01-01T00:10:00Z") } else { Value::Null },
        "head": { "ref": "saka/two-step/s1", "sha": HEAD },
    })
}
