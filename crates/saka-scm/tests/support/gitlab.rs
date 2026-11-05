use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use std::sync::atomic::AtomicI64;

use saka_core::ids::RepoId;
use saka_ports::RepoRef;
use saka_scm::{GitLab, GitLabClient, GitLabConfig};
use serde_json::{Value, json};
use uuid::Uuid;
use wiremock::MockServer;

use super::{HEAD, NOW_MS, NoRepos, TestClock};

pub const TOKEN: &str = "glpat-test-token-0123456789";

pub struct GitLabWorld {
    pub dir: tempfile::TempDir,
    pub server: MockServer,
    pub clock: Arc<TestClock>,
    pub client: Arc<GitLabClient>,
    pub gl: GitLab,
    pub repo: RepoRef,
}

impl GitLabWorld {
    pub async fn new() -> GitLabWorld {
        let dir = tempfile::tempdir().unwrap();
        let token = dir.path().join("token");
        std::fs::write(&token, format!("{TOKEN}\n")).unwrap();
        std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o600)).unwrap();
        let server = MockServer::start().await;
        let clock = Arc::new(TestClock(AtomicI64::new(NOW_MS)));
        let client = Arc::new(
            GitLabClient::new(
                GitLabConfig {
                    integration_id: "gitlab".into(),
                    api_url: server.uri(),
                    web_url: "https://gitlab.example".into(),
                    token_file: token,
                    user_agent: "saka-test".into(),
                },
                clock.clone(),
            )
            .unwrap(),
        );
        let gl = GitLab::new(client.clone(), Arc::new(NoRepos));
        GitLabWorld {
            dir,
            server,
            clock,
            client,
            gl,
            repo: RepoRef {
                repo_id: RepoId(Uuid::from_u128(2)),
                integration_id: "gitlab".into(),
                full_name: "acme/app".into(),
                url: "https://gitlab.example/acme/app".into(),
            },
        }
    }

    pub fn token_path(&self) -> std::path::PathBuf {
        self.dir.path().join("token")
    }
}

pub fn mr(iid: u64, state: &str) -> Value {
    let merged = state == "merged";
    json!({
        "iid": iid,
        "web_url": format!("https://gitlab.example/acme/app/-/merge_requests/{iid}"),
        "state": state,
        "source_branch": "saka/two-step/s1",
        "sha": HEAD,
        "merge_commit_sha": if merged { json!(super::MERGED) } else { Value::Null },
        "squash_commit_sha": Value::Null,
        "merged_at": if merged { json!("2026-01-01T00:10:00Z") } else { Value::Null },
        "detailed_merge_status": if merged { "not_open" } else { "mergeable" },
    })
}
