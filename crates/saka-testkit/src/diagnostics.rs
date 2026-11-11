use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

use saka_core::diagnostics::{
    AgentCli, AgentLogin, AppStatus, FoundCli, Installation, InstalledToolchain, RepoGrant,
    RepoHook, RepoHooks, SupervisorProbe, TokenStatus, ToolchainCache, WebhookStatus,
};
use saka_core::vocab::{HostTool, Tool};
use saka_ports::{PortError, PortResult, Release};
use serde_json::json;

pub const FAKE_CLI_PATH: &str = "/sim/bin/claude";
pub const FAKE_CLI_VERSION: &str = "2.1.0";
pub const FAKE_TOOLCHAINS: &str = "/sim/toolchains";
pub const FAKE_APP_ID: u64 = 4242;
pub const FAKE_APP_SLUG: &str = "saka-sim";
pub const FAKE_INSTALLATION_ID: u64 = 77;
pub const FAKE_TOKEN_NAME: &str = "saka-sim-token";
pub const FAKE_HOOK_ID: u64 = 501;

pub fn full_member() -> RepoGrant {
    RepoGrant::Member {
        access_level: Some(40),
    }
}

pub fn saka_hook(integration_id: &str) -> RepoHook {
    RepoHook {
        id: FAKE_HOOK_ID,
        url: format!("https://saka.example/hooks/{integration_id}"),
        events: [
            "merge_requests_events",
            "note_events",
            "pipeline_events",
            "job_events",
        ]
        .map(String::from)
        .to_vec(),
        ssl_verification: true,
        alert_status: "executable".into(),
        disabled_until: None,
    }
}

pub fn full_installation() -> Installation {
    Installation {
        installed: true,
        installation_id: Some(FAKE_INSTALLATION_ID),
        permissions: [
            ("contents", "write"),
            ("pull_requests", "write"),
            ("checks", "write"),
            ("statuses", "write"),
            ("metadata", "read"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum HostPort {
    Supervisor,
    AgentCli,
    AgentLogin,
    ToolchainCache,
    Releases,
    ScmCredential,
    ScmRepoAccess,
    ScmWebhook,
    ScmRepoWebhook,
    HostVersion,
}

#[derive(Debug, Clone)]
pub struct HostScript {
    pub cli: AgentCli,
    pub login: AgentLogin,
    pub probe: SupervisorProbe,
    pub cache: ToolchainCache,
    pub failing_sources: BTreeSet<Tool>,
    pub app: Result<AppStatus, PortError>,
    pub installations: BTreeMap<String, Installation>,
    pub webhook: Result<WebhookStatus, PortError>,
    pub token: Result<TokenStatus, PortError>,
    pub members: BTreeMap<String, RepoGrant>,
    pub hooks: BTreeMap<String, RepoHooks>,
    pub host_versions: BTreeMap<HostTool, String>,
    pub hang: BTreeSet<HostPort>,
}

impl Default for HostScript {
    fn default() -> HostScript {
        HostScript {
            cli: AgentCli {
                command: "claude".into(),
                path_dirs: vec!["/sim/bin".into()],
                found: Some(FoundCli {
                    path: FAKE_CLI_PATH.into(),
                    version: Some(FAKE_CLI_VERSION.into()),
                }),
            },
            login: AgentLogin::LoggedIn {
                auth_method: "claude.ai".into(),
            },
            probe: SupervisorProbe::Available,
            cache: ToolchainCache {
                path: FAKE_TOOLCHAINS.into(),
                writable: true,
                installed: vec![InstalledToolchain {
                    tool: Tool::Node,
                    version: "22.11.0".into(),
                }],
            },
            failing_sources: BTreeSet::new(),
            app: Ok(AppStatus {
                app_id: FAKE_APP_ID,
                slug: FAKE_APP_SLUG.into(),
                events: ["pull_request", "check_run", "check_suite", "status"]
                    .map(String::from)
                    .to_vec(),
            }),
            installations: BTreeMap::new(),
            webhook: Ok(WebhookStatus {
                url_set: true,
                content_type: Some("json".into()),
                secret_set: true,
                deliveries: Vec::new(),
            }),
            token: Ok(TokenStatus {
                name: FAKE_TOKEN_NAME.into(),
                active: true,
                revoked: false,
                scopes: vec!["api".into()],
                expires_at: None,
            }),
            members: BTreeMap::new(),
            hooks: BTreeMap::new(),
            host_versions: BTreeMap::new(),
            hang: BTreeSet::new(),
        }
    }
}

#[derive(Default)]
pub struct FakeHost {
    script: Mutex<HostScript>,
}

impl FakeHost {
    pub fn script(&self, edit: impl FnOnce(&mut HostScript)) {
        edit(&mut self.script.lock().unwrap());
    }

    async fn answer<T>(&self, port: HostPort, read: impl FnOnce(&HostScript) -> T) -> T {
        let (hangs, value) = {
            let s = self.script.lock().unwrap();
            (s.hang.contains(&port), read(&s))
        };
        if hangs {
            std::future::pending::<()>().await;
        }
        value
    }

    pub(crate) async fn agent_cli(&self) -> PortResult<AgentCli> {
        Ok(self.answer(HostPort::AgentCli, |s| s.cli.clone()).await)
    }

    pub(crate) async fn agent_login(&self) -> PortResult<AgentLogin> {
        Ok(self.answer(HostPort::AgentLogin, |s| s.login.clone()).await)
    }

    pub(crate) async fn probe(&self) -> PortResult<SupervisorProbe> {
        Ok(self.answer(HostPort::Supervisor, |s| s.probe.clone()).await)
    }

    pub(crate) async fn cache(&self) -> PortResult<ToolchainCache> {
        Ok(self
            .answer(HostPort::ToolchainCache, |s| s.cache.clone())
            .await)
    }

    pub(crate) async fn app_status(&self) -> PortResult<AppStatus> {
        self.answer(HostPort::ScmCredential, |s| s.app.clone())
            .await
    }

    pub(crate) async fn installation(&self, full_name: &str) -> PortResult<Installation> {
        Ok(self
            .answer(HostPort::ScmRepoAccess, |s| {
                s.installations
                    .get(full_name)
                    .cloned()
                    .unwrap_or_else(full_installation)
            })
            .await)
    }

    pub(crate) async fn token(&self) -> PortResult<TokenStatus> {
        self.answer(HostPort::ScmCredential, |s| s.token.clone())
            .await
    }

    pub(crate) async fn member(&self, full_name: &str) -> RepoGrant {
        self.answer(HostPort::ScmRepoAccess, |s| {
            s.members
                .get(full_name)
                .cloned()
                .unwrap_or_else(full_member)
        })
        .await
    }

    pub(crate) async fn hooks(&self, full_name: &str) -> RepoHooks {
        self.answer(HostPort::ScmRepoWebhook, |s| {
            s.hooks
                .get(full_name)
                .cloned()
                .unwrap_or_else(|| RepoHooks::Listed(vec![saka_hook(crate::FAKE_INTEGRATION)]))
        })
        .await
    }

    pub(crate) async fn webhook(&self) -> PortResult<WebhookStatus> {
        self.answer(HostPort::ScmWebhook, |s| s.webhook.clone())
            .await
    }

    pub(crate) async fn host_version(&self, tool: HostTool) -> PortResult<Option<String>> {
        Ok(self
            .answer(HostPort::HostVersion, |s| {
                s.host_versions.get(&tool).cloned()
            })
            .await)
    }

    pub(crate) async fn releases(&self, tool: Tool) -> PortResult<Vec<Release>> {
        let failing = self
            .answer(HostPort::Releases, |s| s.failing_sources.contains(&tool))
            .await;
        if failing {
            return Err(PortError::retryable(
                "release_index_unavailable",
                json!({ "tool": tool, "url": format!("https://{tool}.mirror.test/"), "status": 500, "detail": "HTTP 500" }),
            ));
        }
        Ok(vec![Release {
            version: "1.0.0".into(),
            lts: None,
        }])
    }
}
