use std::collections::HashMap;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use saka_core::ids::SessionId;
use saka_ports::{
    Agents, LaunchedSession, Link, PortError, PortResult, Reattached, RunnerEvent, RunnerUnit,
    SessionLaunch, SessionRef, Supervisor, UnitLimits,
};
use serde_json::json;
use tokio::sync::mpsc;

use crate::client::RunnerConnection;
use crate::launch::{self, ClaudeSettings, LaunchSpec, SessionPaths, UnsupportedPath};
use crate::protocol::ServerFrame;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const CONNECT_RETRY: Duration = Duration::from_millis(50);
const VERSION_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone)]
pub struct AgentsConfig {
    pub claude: ClaudeSettings,
    pub sessions_dir: PathBuf,
    pub runtime_dir: PathBuf,
    pub runner_program: PathBuf,
    pub limits: UnitLimits,
    pub known_values: Vec<String>,
    pub server_version: String,
    pub extra_env: Vec<(String, String)>,
}

pub struct ClaudeAgents {
    cfg: AgentsConfig,
    supervisor: Arc<dyn Supervisor>,
    sink: mpsc::UnboundedSender<RunnerEvent>,
    conns: Mutex<HashMap<SessionId, Arc<RunnerConnection>>>,
    generations: Mutex<HashMap<SessionId, u32>>,
}

impl ClaudeAgents {
    fn closed(&self, session: SessionId) -> Link {
        let generation = self
            .generations
            .lock()
            .ok()
            .and_then(|g| g.get(&session).copied())
            .unwrap_or(0);
        Link::Closed { generation }
    }
}

pub fn unit_name(session: SessionId) -> String {
    format!("saka-session-{session}")
}

fn io_error(code: &str, e: &std::io::Error) -> PortError {
    PortError::retryable(code, json!({ "error": e.to_string() }))
}

impl ClaudeAgents {
    pub fn new(
        cfg: AgentsConfig,
        supervisor: Arc<dyn Supervisor>,
        sink: mpsc::UnboundedSender<RunnerEvent>,
    ) -> ClaudeAgents {
        ClaudeAgents {
            cfg,
            supervisor,
            sink,
            conns: Mutex::new(HashMap::new()),
            generations: Mutex::new(HashMap::new()),
        }
    }

    fn drop_conn(&self, session: SessionId) {
        let removed = self
            .conns
            .lock()
            .ok()
            .and_then(|mut c| c.remove(&session))
            .is_some();
        if removed && let Ok(mut g) = self.generations.lock() {
            *g.entry(session).or_insert(0) += 1;
        }
    }

    fn conn(&self, session: &SessionRef) -> PortResult<Arc<RunnerConnection>> {
        self.conns
            .lock()
            .map_err(|_| PortError::permanent("internal", json!({})))?
            .get(&session.session_id)
            .cloned()
            .ok_or_else(|| {
                PortError::retryable(
                    "runner_unreachable",
                    json!({ "session_id": session.session_id }),
                )
            })
    }

    async fn send(&self, session: &SessionRef, frame: ServerFrame) -> PortResult<()> {
        let conn = self.conn(session)?;
        let result = conn.send(session.session_id, &frame).await;
        if result.is_err() {
            self.drop_conn(session.session_id);
        }
        result
    }

    pub fn launch_spec(
        &self,
        program: &Path,
        launch: &SessionLaunch,
    ) -> PortResult<(LaunchSpec, SessionPaths)> {
        let id = launch.session_id.to_string();
        let worktree = std::fs::canonicalize(&launch.environment.worktree_path)
            .map_err(|e| io_error("worktree_unreadable", &e))?;
        let paths = SessionPaths::new(
            &self.cfg.runtime_dir,
            &self.cfg.sessions_dir,
            &id,
            Path::new(&launch.environment.root_path),
            &worktree,
        );
        let mut spec = launch::claude(
            &self.cfg.claude,
            program,
            &id,
            &paths,
            self.cfg.known_values.clone(),
            launch.resume_cli_session_id.as_deref(),
        )
        .map_err(|UnsupportedPath(path)| {
            PortError::permanent(
                "envs_dir_unsupported",
                json!({ "path": path.to_string_lossy() }),
            )
        })?;
        for (k, v) in &self.cfg.extra_env {
            spec.env.insert(k.clone(), v.clone());
        }
        Ok((spec, paths))
    }
}

async fn cli_version(program: &Path) -> Option<String> {
    let output = tokio::time::timeout(
        VERSION_TIMEOUT,
        tokio::process::Command::new(program)
            .arg("--version")
            .env_clear()
            .output(),
    )
    .await
    .ok()?
    .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    text.split_whitespace().next().map(String::from)
}

fn write_launch(paths: &SessionPaths, spec: &LaunchSpec) -> std::io::Result<()> {
    std::fs::create_dir_all(&paths.session_dir)?;
    std::fs::set_permissions(&paths.session_dir, std::fs::Permissions::from_mode(0o700))?;
    for dir in [paths.home(), paths.tmp()] {
        std::fs::create_dir_all(&dir)?;
    }
    let bytes = serde_json::to_vec_pretty(spec).map_err(std::io::Error::other)?;
    let file = paths.launch_file();
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(&file)?;
    std::io::Write::write_all(&mut f, &bytes)?;
    f.sync_all()
}

#[async_trait]
impl Agents for ClaudeAgents {
    async fn start(&self, launch: &SessionLaunch) -> PortResult<LaunchedSession> {
        let program = self.cfg.claude.program().ok_or_else(|| {
            PortError::permanent(
                "agent_cli_not_found",
                json!({ "command": self.cfg.claude.command }),
            )
        })?;
        let version = cli_version(&program).await;
        let (spec, paths) = self.launch_spec(&program, launch)?;
        self.drop_conn(launch.session_id);
        // A socket left by an earlier attempt must not answer for the new runner.
        let _ = std::fs::remove_file(&paths.socket_path);
        write_launch(&paths, &spec).map_err(|e| io_error("launch_write_failed", &e))?;
        let name = unit_name(launch.session_id);
        self.supervisor
            .start(&RunnerUnit {
                name: name.clone(),
                program: self.cfg.runner_program.to_string_lossy().into_owned(),
                args: vec![
                    "runner".into(),
                    "--session".into(),
                    launch.session_id.to_string(),
                    "--launch".into(),
                    paths.launch_file().to_string_lossy().into_owned(),
                ],
                limits: self.cfg.limits.clone(),
            })
            .await?;
        let deadline = tokio::time::Instant::now() + CONNECT_TIMEOUT;
        let conn = loop {
            match RunnerConnection::connect(
                &paths.socket_path,
                launch.session_id,
                0,
                &self.cfg.server_version,
                self.sink.clone(),
            )
            .await
            {
                Ok(conn) => break conn,
                Err(e) if !e.retryable || tokio::time::Instant::now() >= deadline => {
                    let _ = self.supervisor.stop(&name).await;
                    return Err(e);
                }
                Err(_) => tokio::time::sleep(CONNECT_RETRY).await,
            }
        };
        if let Ok(mut conns) = self.conns.lock() {
            conns.insert(launch.session_id, Arc::new(conn));
        }
        Ok(LaunchedSession {
            cli: "claude".into(),
            cli_version: version,
            model: self.cfg.claude.model.clone().filter(|m| !m.is_empty()),
            account: self.cfg.claude.account.clone(),
            runner_unit: name,
            socket_path: spec.socket_path,
            log_path: spec.log_path,
        })
    }

    async fn send_turn(&self, session: &SessionRef, turn: u32, text: &str) -> PortResult<()> {
        self.send(
            session,
            ServerFrame::Turn {
                turn,
                text: text.to_string(),
            },
        )
        .await
    }

    async fn answer_permission(
        &self,
        session: &SessionRef,
        request_id: &str,
        allow: bool,
        message: Option<&str>,
    ) -> PortResult<()> {
        self.send(
            session,
            ServerFrame::PermissionAnswer {
                request_id: request_id.to_string(),
                behavior: if allow { "allow" } else { "deny" }.to_string(),
                message: message.map(String::from),
            },
        )
        .await
    }

    async fn interrupt(&self, session: &SessionRef) -> PortResult<()> {
        self.send(session, ServerFrame::Interrupt).await
    }

    async fn steer(&self, session: &SessionRef, n: u32, text: &str) -> PortResult<()> {
        self.send(
            session,
            ServerFrame::Steer {
                n,
                text: text.to_string(),
            },
        )
        .await
    }

    async fn reattach(&self, session: &SessionRef, from_offset: u64) -> PortResult<Reattached> {
        self.drop_conn(session.session_id);
        let conn = RunnerConnection::connect(
            Path::new(&session.socket_path),
            session.session_id,
            from_offset,
            &self.cfg.server_version,
            self.sink.clone(),
        )
        .await?;
        let reattached = Reattached {
            runner_version: conn.runner_version.clone(),
            next_offset: conn.next_offset,
        };
        if let Ok(mut conns) = self.conns.lock() {
            conns.insert(session.session_id, Arc::new(conn));
        }
        Ok(reattached)
    }

    async fn unit_active(&self, session: &SessionRef) -> PortResult<bool> {
        self.supervisor.is_active(&session.runner_unit).await
    }

    fn link(&self, session: SessionId) -> Link {
        let conn = self
            .conns
            .lock()
            .ok()
            .and_then(|c| c.get(&session).cloned());
        match conn {
            Some(c) if !c.is_closed() => Link::Open {
                silent_ms: c.silent_ms(),
            },
            Some(_) => {
                self.drop_conn(session);
                self.closed(session)
            }
            None => self.closed(session),
        }
    }

    fn disconnect(&self, session: SessionId) {
        self.drop_conn(session);
    }

    async fn stop(&self, session_id: SessionId, runner_unit: Option<&str>) -> PortResult<()> {
        let conn = self
            .conns
            .lock()
            .ok()
            .and_then(|c| c.get(&session_id).cloned());
        self.drop_conn(session_id);
        if let Some(conn) = conn {
            let _ = conn.send(session_id, &ServerFrame::Shutdown).await;
        }
        let name = runner_unit
            .map(String::from)
            .unwrap_or_else(|| unit_name(session_id));
        self.supervisor.stop(&name).await
    }
}
