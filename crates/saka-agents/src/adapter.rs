use std::collections::HashMap;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use saka_core::ids::SessionId;
use saka_ports::{
    Agents, LaunchProfile, LaunchedSession, Link, PortError, PortResult, Reattached, RunnerEvent,
    RunnerUnit, SessionLaunch, SessionRef, Supervisor, UnitLimits,
};
use serde_json::json;
use tokio::sync::mpsc;

use crate::client::RunnerConnection;
use crate::launch::{
    self, ClaudeSettings, LaunchOptions, LaunchSpec, SessionPaths, UnsupportedPath,
};
use crate::protocol::ServerFrame;
use crate::secrets::SessionSecrets;

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
        let (cwd, tools) = match &launch.profile {
            LaunchProfile::Task => (worktree.clone(), None),
            LaunchProfile::Architect { cwd } => {
                let dir = std::fs::canonicalize(worktree.join(cwd))
                    .map_err(|e| io_error("worktree_unreadable", &e))?;
                if !dir.starts_with(&worktree) {
                    return Err(PortError::permanent(
                        "session_cwd_invalid",
                        json!({ "cwd": cwd }),
                    ));
                }
                (dir, Some(launch::ARCHITECT_TOOLS))
            }
        };
        let paths = SessionPaths::new(
            &self.cfg.runtime_dir,
            &self.cfg.sessions_dir,
            &id,
            Path::new(&launch.environment.root_path),
            &worktree,
        );
        let add_dirs = match launch.profile {
            LaunchProfile::Task => launch
                .environment
                .extra_dirs
                .iter()
                .map(|dir| {
                    std::fs::canonicalize(&dir.path)
                        .map_err(|e| io_error("docs_dir_unreadable", &e))
                })
                .collect::<PortResult<Vec<_>>>()?,
            LaunchProfile::Architect { .. } => Vec::new(),
        };
        let options = LaunchOptions {
            cwd: &cwd,
            tools,
            model: launch.model.as_deref(),
            resume: launch.resume_cli_session_id.as_deref(),
            author: &launch.author,
            add_dirs: &add_dirs,
        };
        let mut spec = launch::claude(
            &self.cfg.claude,
            program,
            &id,
            &paths,
            self.cfg.known_values.clone(),
            &options,
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
        // An architect session reads docs: no recipe env, ports, toolchains, or secrets reach it.
        if launch.profile != LaunchProfile::Task {
            return Ok((spec, paths));
        }
        for (k, v) in &launch.environment.vars {
            spec.env.entry(k.clone()).or_insert_with(|| v.clone());
        }
        if !launch.environment.secrets.is_empty() {
            spec.secrets_path = Some(paths.secrets_file().to_string_lossy().into_owned());
        }
        if !launch.environment.path_prefix.is_empty()
            && let Some(path) = spec.env.get_mut("PATH")
        {
            *path = format!("{}:{path}", launch.environment.path_prefix.join(":"));
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
        let secrets = paths.secrets_file();
        if spec.secrets_path.is_some() {
            SessionSecrets::of(&launch.environment.secrets)
                .write(&secrets)
                .map_err(|e| io_error("launch_write_failed", &e))?;
        }
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
                    let _ = std::fs::remove_file(&secrets);
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

#[cfg(test)]
mod tests {
    use saka_core::model::RunOwner;
    use saka_core::secret::{SecretText, SecretVar};
    use saka_core::vocab::SecretPurpose;
    use saka_ports::{CommandOutcome, CommandUnit, EnvRef, RepoRef};
    use serde::de::DeserializeOwned;

    use super::*;

    struct NoSupervisor;

    #[async_trait]
    impl Supervisor for NoSupervisor {
        async fn start(&self, _unit: &RunnerUnit) -> PortResult<()> {
            Ok(())
        }
        async fn stop(&self, _name: &str) -> PortResult<()> {
            Ok(())
        }
        async fn is_active(&self, _name: &str) -> PortResult<bool> {
            Ok(false)
        }
        async fn run(&self, _unit: &CommandUnit) -> PortResult<CommandOutcome> {
            Err(PortError::permanent("not_used", json!({})))
        }
    }

    fn id<T: DeserializeOwned>(n: u8) -> T {
        serde_json::from_value(json!(format!("00000000-0000-0000-0000-0000000000{n:02}")))
            .unwrap_or_else(|e| panic!("{e}"))
    }

    fn agents(dir: &Path) -> ClaudeAgents {
        let (sink, _events) = mpsc::unbounded_channel();
        ClaudeAgents::new(
            AgentsConfig {
                claude: ClaudeSettings {
                    command: "claude".into(),
                    account: "default".into(),
                    account_dir: "/data/agents/claude/default".into(),
                    model: None,
                    path: vec!["/usr/bin".into(), "/bin".into()],
                    committer: saka_core::authorship::GitIdentity {
                        name: "Saka Agent".into(),
                        email: "agent@saka.invalid".into(),
                    },
                },
                sessions_dir: dir.join("sessions"),
                runtime_dir: dir.join("run"),
                runner_program: "/usr/bin/saka".into(),
                limits: UnitLimits {
                    memory_max: "1G".into(),
                    cpu_quota: "100%".into(),
                    tasks_max: 64,
                },
                known_values: Vec::new(),
                server_version: "0.0.0".into(),
                extra_env: Vec::new(),
            },
            Arc::new(NoSupervisor),
            sink,
        )
    }

    fn launch(root: &Path, profile: LaunchProfile, model: Option<&str>) -> SessionLaunch {
        let vars = [
            ("APP_ENV", "test"),
            ("PORT", "41000"),
            ("SAKA_ENVIRONMENT_ID", "e-1"),
            ("SAKA_PORT_BASE", "41000"),
            ("SAKA_PORT_COUNT", "3"),
            ("SAKA_TASK_KEY", "a/s1"),
            ("HOME", "/somewhere/else"),
        ];
        SessionLaunch {
            session_id: id(1),
            owner: RunOwner::StageRun {
                work_item_id: id(2),
                stage_run_id: id(5),
            },
            environment: EnvRef {
                environment_id: id(3),
                repo: RepoRef {
                    repo_id: id(4),
                    integration_id: "github".into(),
                    full_name: "acme/app".into(),
                    url: "https://github.com/acme/app".into(),
                },
                branch: Some("saka/a/s1".into()),
                root_path: root.to_string_lossy().into_owned(),
                worktree_path: root.join("worktree").to_string_lossy().into_owned(),
                vars: vars
                    .iter()
                    .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                    .collect(),
                path_prefix: vec![
                    "/data/toolchains/go/1.26.3-linux-x64/bin".into(),
                    "/data/toolchains/node/26.13.0-linux-x64/bin".into(),
                ],
                secrets: vec![SecretVar {
                    name: "API_TOKEN".into(),
                    value: SecretText::new("s3cr3t".into()),
                    purposes: vec![SecretPurpose::Session],
                }],
                extra_dirs: Vec::new(),
            },
            resume_cli_session_id: None,
            profile,
            model: model.map(String::from),
            author: saka_core::authorship::GitIdentity {
                name: "Octo Cat".into(),
                email: "1+octo@users.noreply.github.com".into(),
            },
        }
    }

    #[test]
    fn the_session_env_is_the_allowlist_plus_the_recipe_env_and_nothing_else() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let root = dir.path().join("e-1");
        std::fs::create_dir_all(root.join("worktree")).unwrap_or_else(|e| panic!("{e}"));
        let agents = agents(dir.path());
        let launch = launch(&root, LaunchProfile::Task, None);
        let (spec, _paths) = agents
            .launch_spec(Path::new("/usr/bin/claude"), &launch)
            .unwrap_or_else(|e| panic!("{e:?}"));
        let keys: Vec<&str> = spec.env.keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            [
                "APP_ENV",
                "CLAUDE_CONFIG_DIR",
                "GIT_AUTHOR_EMAIL",
                "GIT_AUTHOR_NAME",
                "GIT_COMMITTER_EMAIL",
                "GIT_COMMITTER_NAME",
                "HOME",
                "LANG",
                "PATH",
                "PORT",
                "SAKA_ENVIRONMENT_ID",
                "SAKA_PORT_BASE",
                "SAKA_PORT_COUNT",
                "SAKA_TASK_KEY",
                "TERM",
                "TMPDIR",
            ]
        );
        assert_eq!(spec.env["SAKA_PORT_BASE"], "41000");
        assert_eq!(
            spec.env["PATH"],
            "/data/toolchains/go/1.26.3-linux-x64/bin:/data/toolchains/node/26.13.0-linux-x64/bin:/usr/bin:/bin"
        );
        assert_eq!(
            spec.env["HOME"],
            root.join("home").to_string_lossy(),
            "a recipe var never replaces a Saka-owned one"
        );
    }

    #[test]
    fn a_task_session_gets_the_docs_checkout_as_an_added_dir_and_its_path_in_the_env() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let root = dir
            .path()
            .canonicalize()
            .unwrap_or_else(|e| panic!("{e}"))
            .join("e-1");
        std::fs::create_dir_all(root.join("worktree")).unwrap_or_else(|e| panic!("{e}"));
        let docs = root.join("docs/docs");
        std::fs::create_dir_all(&docs).unwrap_or_else(|e| panic!("{e}"));
        let agents = agents(dir.path());
        let mut task = launch(&root, LaunchProfile::Task, None);
        let docs_dir = docs.to_string_lossy().into_owned();
        task.environment.extra_dirs = vec![saka_ports::ExtraDir {
            path: docs_dir.clone(),
            writable: true,
        }];
        task.environment
            .vars
            .insert("SAKA_DOCS_DIR".into(), docs_dir.clone());
        let (spec, _paths) = agents
            .launch_spec(Path::new("/usr/bin/claude"), &task)
            .unwrap_or_else(|e| panic!("{e:?}"));
        let at = spec
            .args
            .iter()
            .position(|a| a == "--add-dir")
            .unwrap_or_else(|| panic!("no --add-dir in {:?}", spec.args));
        assert_eq!(spec.args[at + 1], docs_dir);
        assert_eq!(spec.env["SAKA_DOCS_DIR"], docs_dir);

        let mut architect = launch(&root, LaunchProfile::Architect { cwd: ".".into() }, None);
        architect.environment.extra_dirs = vec![saka_ports::ExtraDir {
            path: docs_dir,
            writable: true,
        }];
        let (spec, _paths) = agents
            .launch_spec(Path::new("/usr/bin/claude"), &architect)
            .unwrap_or_else(|e| panic!("{e:?}"));
        assert!(!spec.args.contains(&"--add-dir".to_string()));

        task.environment.extra_dirs = vec![saka_ports::ExtraDir {
            path: root.join("gone").to_string_lossy().into_owned(),
            writable: true,
        }];
        let missing = agents
            .launch_spec(Path::new("/usr/bin/claude"), &task)
            .unwrap_err();
        assert_eq!(missing.code, "docs_dir_unreadable");
    }

    #[test]
    fn an_architect_launch_reads_from_the_project_folder_with_only_the_doc_tools() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let root = dir
            .path()
            .canonicalize()
            .unwrap_or_else(|e| panic!("{e}"))
            .join("e-1");
        let worktree = root.join("worktree");
        std::fs::create_dir_all(worktree.join("docs")).unwrap_or_else(|e| panic!("{e}"));
        let agents = agents(dir.path());
        let architect = launch(
            &root,
            LaunchProfile::Architect { cwd: "docs".into() },
            Some("opus"),
        );
        let (spec, _paths) = agents
            .launch_spec(Path::new("/usr/bin/claude"), &architect)
            .unwrap_or_else(|e| panic!("{e:?}"));
        let settings = format!(
            "{{\"attribution\":{{\"commit\":\"\",\"pr\":\"\"}},\"claudeMdExcludes\":[\"!{}/**\"]}}",
            worktree.display()
        );
        assert_eq!(
            spec.args,
            [
                "-p",
                "--input-format",
                "stream-json",
                "--output-format",
                "stream-json",
                "--verbose",
                "--permission-mode",
                "manual",
                "--permission-prompt-tool",
                "stdio",
                "--setting-sources",
                "project",
                "--strict-mcp-config",
                "--settings",
                &settings,
                "--tools",
                "Read,Glob,Grep,LS,TodoWrite,Write,Edit,MultiEdit",
                "--model",
                "opus",
            ]
        );
        assert_eq!(spec.cwd, worktree.join("docs").to_string_lossy());
        let keys: Vec<&str> = spec.env.keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            [
                "CLAUDE_CONFIG_DIR",
                "GIT_AUTHOR_EMAIL",
                "GIT_AUTHOR_NAME",
                "GIT_COMMITTER_EMAIL",
                "GIT_COMMITTER_NAME",
                "HOME",
                "LANG",
                "PATH",
                "TERM",
                "TMPDIR",
            ]
        );
        assert_eq!(spec.env["PATH"], "/usr/bin:/bin");
        assert_eq!(spec.secrets_path, None);

        let root_cwd = launch(&root, LaunchProfile::Architect { cwd: String::new() }, None);
        let (spec, _paths) = agents
            .launch_spec(Path::new("/usr/bin/claude"), &root_cwd)
            .unwrap_or_else(|e| panic!("{e:?}"));
        assert_eq!(spec.cwd, worktree.to_string_lossy());
        assert!(!spec.args.contains(&"--model".to_string()));

        let task = launch(&root, LaunchProfile::Task, None);
        let (spec, _paths) = agents
            .launch_spec(Path::new("/usr/bin/claude"), &task)
            .unwrap_or_else(|e| panic!("{e:?}"));
        assert!(!spec.args.contains(&"--tools".to_string()));
        assert_eq!(spec.cwd, worktree.to_string_lossy());

        let escape = launch(&root, LaunchProfile::Architect { cwd: "..".into() }, None);
        let refused = agents
            .launch_spec(Path::new("/usr/bin/claude"), &escape)
            .err()
            .unwrap_or_else(|| panic!("an architect cwd outside the worktree must be refused"));
        assert_eq!(refused.code, "session_cwd_invalid");
    }
}
