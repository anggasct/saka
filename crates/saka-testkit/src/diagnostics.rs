use std::collections::BTreeSet;
use std::sync::Mutex;

use saka_core::diagnostics::{
    AgentCli, AgentLogin, FoundCli, InstalledToolchain, SupervisorProbe, ToolchainCache,
};
use saka_core::vocab::Tool;
use saka_ports::{PortError, PortResult, Release};
use serde_json::json;

pub const FAKE_CLI_PATH: &str = "/sim/bin/claude";
pub const FAKE_CLI_VERSION: &str = "2.1.0";
pub const FAKE_TOOLCHAINS: &str = "/sim/toolchains";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum HostPort {
    Supervisor,
    AgentCli,
    AgentLogin,
    ToolchainCache,
    Releases,
}

#[derive(Debug, Clone)]
pub struct HostScript {
    pub cli: AgentCli,
    pub login: AgentLogin,
    pub probe: SupervisorProbe,
    pub cache: ToolchainCache,
    pub failing_sources: BTreeSet<Tool>,
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
