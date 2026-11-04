use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Mutex;
use std::time::Duration;

use std::io::Read;

use async_trait::async_trait;
use saka_core::diagnostics::SupervisorProbe;
use saka_core::secret::SecretText;
use saka_ports::{
    COMMAND_OUTPUT_MAX, CommandOutcome, CommandUnit, PortError, PortResult, RunnerUnit, Supervisor,
};
use serde_json::json;
use tokio::process::{Child, Command};
use zeroize::Zeroizing;

const STOP_WAIT: Duration = Duration::from_secs(10);
const KILL_WAIT: Duration = Duration::from_secs(5);
const POLL: Duration = Duration::from_millis(100);

const RUN_BACKSTOP: Duration = Duration::from_secs(30);

pub const PROBE_TIMEOUT: Duration = Duration::from_secs(10);
const PROBE_DETAIL_MAX: usize = 512;

const SOURCE_SECRETS: &str = r#"set -a; . "$0" || exit 125; set +a; rm -f -- "$0"; exec "$@""#;

pub fn runtime_dir() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR").map_or_else(
        || PathBuf::from(format!("/run/user/{}", rustix::process::getuid().as_raw())),
        PathBuf::from,
    )
}

fn push_quoted(out: &mut String, value: &str) {
    out.push('\'');
    for c in value.chars() {
        if c == '\'' {
            out.push_str(r"'\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
}

struct SecretFile {
    path: PathBuf,
}

impl SecretFile {
    fn write(unit: &str, vars: &[(String, SecretText)]) -> std::io::Result<Option<SecretFile>> {
        use std::io::Write;
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
        if vars.is_empty() {
            return Ok(None);
        }
        let dir = runtime_dir().join("saka").join("secrets");
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&dir)?;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
        let path = dir.join(format!("{unit}.env"));
        let _ = std::fs::remove_file(&path);
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)?;
        let file_guard = SecretFile { path };
        // Sized up front so growing the buffer never leaves an unwiped copy behind.
        let size = vars
            .iter()
            .map(|(n, v)| n.len() + v.expose().len() * 4 + 4)
            .sum();
        let mut text = Zeroizing::new(String::with_capacity(size));
        for (name, value) in vars {
            text.push_str(name);
            text.push('=');
            push_quoted(&mut text, value.expose());
            text.push('\n');
        }
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        Ok(Some(file_guard))
    }
}

impl Drop for SecretFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn failed(code: &str, detail: impl Into<String>) -> PortError {
    PortError::retryable(code, json!({ "detail": detail.into() }))
}

struct Captured {
    output: Vec<u8>,
    truncated: bool,
}

fn keep_tail(buf: &mut Vec<u8>, truncated: &mut bool) {
    if buf.len() > COMMAND_OUTPUT_MAX {
        let cut = buf.len() - COMMAND_OUTPUT_MAX;
        buf.drain(..cut);
        *truncated = true;
    }
}

fn read_tail(mut reader: std::io::PipeReader) -> Captured {
    let mut output = Vec::new();
    let mut truncated = false;
    let mut chunk = [0u8; 64 * 1024];
    loop {
        match reader.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                output.extend_from_slice(&chunk[..n]);
                keep_tail(&mut output, &mut truncated);
            }
        }
    }
    Captured { output, truncated }
}

// stdout and stderr share one pipe so the log keeps the order the command wrote them in.
fn merged_pipe() -> PortResult<(std::io::PipeReader, Stdio, Stdio)> {
    let (reader, writer) =
        std::io::pipe().map_err(|e| failed("supervisor_run_failed", e.to_string()))?;
    let err = writer
        .try_clone()
        .map_err(|e| failed("supervisor_run_failed", e.to_string()))?;
    Ok((reader, Stdio::from(writer), Stdio::from(err)))
}

async fn collect(
    reader: std::io::PipeReader,
    exit_code: Option<i32>,
    timed_out: bool,
) -> PortResult<CommandOutcome> {
    let captured = tokio::task::spawn_blocking(move || read_tail(reader))
        .await
        .map_err(|e| failed("supervisor_run_failed", e.to_string()))?;
    Ok(CommandOutcome {
        exit_code,
        timed_out,
        output: captured.output,
        truncated: captured.truncated,
    })
}

pub async fn probe_systemd(program: &std::ffi::OsStr, timeout: Duration) -> SupervisorProbe {
    let run = Command::new(program)
        .args([
            "--user",
            "--wait",
            "--pipe",
            "--quiet",
            "--collect",
            "--",
            "/bin/true",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .output();
    let unavailable = |exit_code, detail: &str| {
        let detail = detail.trim();
        let cut = (0..=detail.len().min(PROBE_DETAIL_MAX))
            .rev()
            .find(|i| detail.is_char_boundary(*i))
            .unwrap_or(0);
        SupervisorProbe::Unavailable {
            exit_code,
            detail: detail[..cut].to_string(),
        }
    };
    match tokio::time::timeout(timeout, run).await {
        Ok(Ok(out)) if out.status.success() => SupervisorProbe::Available,
        Ok(Ok(out)) => unavailable(out.status.code(), &String::from_utf8_lossy(&out.stderr)),
        Ok(Err(e)) => unavailable(None, &e.to_string()),
        Err(_) => unavailable(None, "timed out"),
    }
}

#[derive(Debug, Clone, Default)]
pub struct SystemdSupervisor;

impl SystemdSupervisor {
    async fn systemctl(args: &[&str]) -> std::io::Result<std::process::Output> {
        Command::new("systemctl")
            .arg("--user")
            .args(args)
            .stdin(Stdio::null())
            .output()
            .await
    }

    fn service(name: &str) -> String {
        format!("{name}.service")
    }

    pub async fn is_active(name: &str) -> bool {
        Self::systemctl(&["is-active", "--quiet", &Self::service(name)])
            .await
            .is_ok_and(|o| o.status.success())
    }

    pub async fn property(name: &str, property: &str) -> Option<String> {
        let out = Self::systemctl(&["show", "-p", property, "--value", &Self::service(name)])
            .await
            .ok()?;
        Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    pub async fn processes(name: &str) -> Vec<u32> {
        let Some(cgroup) = Self::property(name, "ControlGroup")
            .await
            .filter(|c| !c.is_empty())
        else {
            return Vec::new();
        };
        std::fs::read_to_string(format!("/sys/fs/cgroup{cgroup}/cgroup.procs"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.trim().parse().ok())
            .collect()
    }

    async fn wait_gone(name: &str, wait: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            if !Self::is_active(name).await && Self::processes(name).await.is_empty() {
                return true;
            }
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(POLL).await;
        }
    }
}

#[async_trait]
impl Supervisor for SystemdSupervisor {
    async fn start(&self, unit: &RunnerUnit) -> PortResult<()> {
        if Self::is_active(&unit.name).await {
            self.stop(&unit.name).await?;
        }
        // A failed unit with the same name blocks a new one until it is reset.
        let _ = Self::systemctl(&["reset-failed", &Self::service(&unit.name)]).await;
        let out = Command::new("systemd-run")
            .args(["--user", "--collect", "--quiet"])
            .arg(format!("--unit={}", unit.name))
            .args(["-p", &format!("MemoryMax={}", unit.limits.memory_max)])
            .args(["-p", &format!("CPUQuota={}", unit.limits.cpu_quota)])
            .args(["-p", &format!("TasksMax={}", unit.limits.tasks_max)])
            .args(["-p", "KillMode=control-group"])
            .arg("--")
            .arg(&unit.program)
            .args(&unit.args)
            .stdin(Stdio::null())
            .output()
            .await
            .map_err(|e| failed("supervisor_start_failed", e.to_string()))?;
        if !out.status.success() {
            return Err(failed(
                "supervisor_start_failed",
                String::from_utf8_lossy(&out.stderr),
            ));
        }
        Ok(())
    }

    async fn stop(&self, name: &str) -> PortResult<()> {
        let service = Self::service(name);
        let _ = tokio::time::timeout(STOP_WAIT, Self::systemctl(&["stop", &service])).await;
        if Self::wait_gone(name, Duration::ZERO).await {
            return Ok(());
        }
        let _ = Self::systemctl(&["kill", "--signal=SIGKILL", &service]).await;
        if Self::wait_gone(name, KILL_WAIT).await {
            Ok(())
        } else {
            Err(failed(
                "supervisor_stop_failed",
                format!("{service} still has processes"),
            ))
        }
    }

    async fn is_active(&self, name: &str) -> PortResult<bool> {
        Ok(Self::is_active(name).await)
    }

    async fn run(&self, unit: &CommandUnit) -> PortResult<CommandOutcome> {
        if Self::is_active(&unit.name).await {
            self.stop(&unit.name).await?;
        }
        let _ = Self::systemctl(&["reset-failed", &Self::service(&unit.name)]).await;
        let (reader, stdout, stderr) = merged_pipe()?;
        let backstop = (unit.timeout + RUN_BACKSTOP).as_secs();
        let mut command = Command::new("systemd-run");
        command
            .args(["--user", "--wait", "--pipe", "--collect", "--quiet"])
            .arg(format!("--unit={}", unit.name))
            .arg(format!("--working-directory={}", unit.cwd))
            .args(["-p", &format!("MemoryMax={}", unit.limits.memory_max)])
            .args(["-p", &format!("CPUQuota={}", unit.limits.cpu_quota)])
            .args(["-p", &format!("TasksMax={}", unit.limits.tasks_max)])
            .args(["-p", &format!("RuntimeMaxSec={backstop}")])
            .args(["-p", "KillMode=control-group"])
            // The user manager's environment would leak into the unit; `env -i` starts from nothing.
            .args(["--", "/usr/bin/env", "-i"]);
        for (key, value) in &unit.env {
            command.arg(format!("{key}={value}"));
        }
        let secrets = SecretFile::write(&unit.name, &unit.secret_env)
            .map_err(|e| failed("supervisor_run_failed", e.to_string()))?;
        if let Some(file) = &secrets {
            // Secret values never reach argv: the shell sources them from a 0600 file and removes it.
            command
                .args(["/bin/sh", "-c", SOURCE_SECRETS])
                .arg(&file.path);
        }
        let mut child = command
            .arg(&unit.program)
            .args(&unit.args)
            .stdin(Stdio::null())
            .stdout(stdout)
            .stderr(stderr)
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| failed("supervisor_run_failed", e.to_string()))?;
        drop(command);
        let result = match tokio::time::timeout(unit.timeout, child.wait()).await {
            Ok(status) => {
                let status = status.map_err(|e| failed("supervisor_run_failed", e.to_string()))?;
                collect(reader, status.code(), false).await
            }
            Err(_) => {
                self.stop(&unit.name).await?;
                let _ = child.wait().await;
                collect(reader, None, true).await
            }
        };
        drop(secrets);
        result
    }

    async fn probe(&self) -> PortResult<SupervisorProbe> {
        Ok(probe_systemd(std::ffi::OsStr::new("systemd-run"), PROBE_TIMEOUT).await)
    }
}

pub struct DirectSupervisor {
    log_dir: PathBuf,
    children: Mutex<HashMap<String, Child>>,
}

impl DirectSupervisor {
    pub fn new(log_dir: PathBuf) -> DirectSupervisor {
        DirectSupervisor {
            log_dir,
            children: Mutex::new(HashMap::new()),
        }
    }

    pub fn log_path(&self, name: &str) -> PathBuf {
        self.log_dir.join(format!("{name}.log"))
    }

    fn take(&self, name: &str) -> Option<Child> {
        self.children.lock().ok()?.remove(name)
    }

    pub fn pid(&self, name: &str) -> Option<u32> {
        self.children.lock().ok()?.get(name).and_then(Child::id)
    }
}

#[async_trait]
impl Supervisor for DirectSupervisor {
    async fn start(&self, unit: &RunnerUnit) -> PortResult<()> {
        self.stop(&unit.name).await?;
        std::fs::create_dir_all(&self.log_dir)
            .map_err(|e| failed("supervisor_start_failed", e.to_string()))?;
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.log_path(&unit.name))
            .map_err(|e| failed("supervisor_start_failed", e.to_string()))?;
        let err = log
            .try_clone()
            .map_err(|e| failed("supervisor_start_failed", e.to_string()))?;
        let child = Command::new(&unit.program)
            .args(&unit.args)
            .stdin(Stdio::null())
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(err))
            .process_group(0)
            .spawn()
            .map_err(|e| failed("supervisor_start_failed", e.to_string()))?;
        if let Ok(mut children) = self.children.lock() {
            children.insert(unit.name.clone(), child);
        }
        Ok(())
    }

    async fn stop(&self, name: &str) -> PortResult<()> {
        let Some(mut child) = self.take(name) else {
            return Ok(());
        };
        let group = child
            .id()
            .and_then(|p| rustix::process::Pid::from_raw(p as i32));
        if let Some(pid) = group {
            let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::TERM);
        }
        if tokio::time::timeout(STOP_WAIT, child.wait()).await.is_err() {
            if let Some(pid) = group {
                let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
            }
            let _ = child.wait().await;
        }
        Ok(())
    }

    async fn is_active(&self, name: &str) -> PortResult<bool> {
        let mut children = self
            .children
            .lock()
            .map_err(|_| failed("supervisor_poisoned", "children lock"))?;
        Ok(children
            .get_mut(name)
            .is_some_and(|c| matches!(c.try_wait(), Ok(None))))
    }

    async fn run(&self, unit: &CommandUnit) -> PortResult<CommandOutcome> {
        let (reader, stdout, stderr) = merged_pipe()?;
        let mut command = Command::new(&unit.program);
        command
            .args(&unit.args)
            .current_dir(&unit.cwd)
            .env_clear()
            .envs(&unit.env)
            .envs(unit.secret_env.iter().map(|(k, v)| (k, v.expose())))
            .stdin(Stdio::null())
            .stdout(stdout)
            .stderr(stderr)
            .process_group(0);
        let mut child = command
            .spawn()
            .map_err(|e| failed("supervisor_run_failed", e.to_string()))?;
        drop(command);
        let group = child
            .id()
            .and_then(|p| rustix::process::Pid::from_raw(p as i32));
        match tokio::time::timeout(unit.timeout, child.wait()).await {
            Ok(status) => {
                let status = status.map_err(|e| failed("supervisor_run_failed", e.to_string()))?;
                // Background children left by the command still hold the pipe open.
                if let Some(pid) = group {
                    let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
                }
                collect(reader, status.code(), false).await
            }
            Err(_) => {
                if let Some(pid) = group {
                    let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
                }
                let _ = child.wait().await;
                collect(reader, None, true).await
            }
        }
    }

    async fn probe(&self) -> PortResult<SupervisorProbe> {
        Ok(SupervisorProbe::Available)
    }
}
