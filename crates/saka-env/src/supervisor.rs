//! Supervisors for runner units.
//!
//! - [`SystemdSupervisor`]: a transient `systemd --user` service per session, outside the
//!   server's cgroup, with `MemoryMax`, `CPUQuota`, and `TasksMax`.
//! - [`DirectSupervisor`]: a child process in its own process group (tests only).

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use saka_ports::{PortError, PortResult, RunnerUnit, Supervisor};
use serde_json::json;
use tokio::process::{Child, Command};

const STOP_WAIT: Duration = Duration::from_secs(10);
const KILL_WAIT: Duration = Duration::from_secs(5);
const POLL: Duration = Duration::from_millis(100);

fn failed(code: &str, detail: impl Into<String>) -> PortError {
    PortError::retryable(code, json!({ "detail": detail.into() }))
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

    /// A unit property as `systemctl show` prints it.
    pub async fn property(name: &str, property: &str) -> Option<String> {
        let out = Self::systemctl(&["show", "-p", property, "--value", &Self::service(name)])
            .await
            .ok()?;
        Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    /// Processes in the unit's cgroup (empty when the unit is gone).
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
}

/// Runs each runner as a child in its own process group, logging to `log_dir/<name>.log`.
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

    /// The runner's pid, while it runs.
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
}
