use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use tokio::process::{Child, Command};
use tokio::sync::{Mutex, watch};

use crate::error::ShellError;

pub const ALLOWED_ORIGIN: &str = "tauri://localhost";

const INHERITED_ENV: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "LANG",
    "LC_ALL",
    "TMPDIR",
    "XDG_RUNTIME_DIR",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "DBUS_SESSION_BUS_ADDRESS",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Stopped,
    Starting,
    Running,
    Exited,
}

#[derive(Debug, Clone)]
pub struct Timing {
    pub ready_within: Duration,
    pub poll_every: Duration,
    pub term_grace: Duration,
}

impl Default for Timing {
    fn default() -> Timing {
        Timing {
            ready_within: Duration::from_secs(30),
            poll_every: Duration::from_millis(200),
            term_grace: Duration::from_secs(10),
        }
    }
}

struct Inner {
    program: PathBuf,
    dir: PathBuf,
    timing: Timing,
    http: reqwest::Client,
    child: Mutex<Option<Child>>,
    phase: watch::Sender<Phase>,
}

#[derive(Clone)]
pub struct Sidecar(Arc<Inner>);

pub fn bundled_program() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("saka")))
        .unwrap_or_else(|| PathBuf::from("saka"))
}

pub fn config_text(port: u16, data_dir: &Path) -> Result<String, ShellError> {
    let data = data_dir.to_str().ok_or(ShellError::LocalStartFailed)?;
    let unsafe_char = |c: char| c.is_control() || "\"\\*?[]{}()!+@|".contains(c);
    if data.chars().any(unsafe_char) {
        return Err(ShellError::LocalStartFailed);
    }
    Ok(format!(
        "[server]\nlisten = \"127.0.0.1:{port}\"\nallowed_origins = [\"{ALLOWED_ORIGIN}\"]\ndata_dir = \"{data}\"\n"
    ))
}

impl Sidecar {
    pub fn new(program: PathBuf, dir: PathBuf, http: reqwest::Client, timing: Timing) -> Sidecar {
        Sidecar(Arc::new(Inner {
            program,
            dir,
            timing,
            http,
            child: Mutex::new(None),
            phase: watch::channel(Phase::Stopped).0,
        }))
    }

    pub fn phase(&self) -> Phase {
        *self.0.phase.borrow()
    }

    pub fn watch(&self) -> watch::Receiver<Phase> {
        self.0.phase.subscribe()
    }

    pub async fn start(&self, port: u16) -> Result<(), ShellError> {
        let mut slot = self.0.child.lock().await;
        if slot.is_some() && self.phase() == Phase::Running {
            return Ok(());
        }
        self.0.phase.send_replace(Phase::Starting);
        let outcome = self.launch(port).await;
        match outcome {
            Ok(child) => {
                *slot = Some(child);
                self.0.phase.send_replace(Phase::Running);
                self.watch_exit();
                Ok(())
            }
            Err(e) => {
                self.0.phase.send_replace(Phase::Exited);
                Err(e)
            }
        }
    }

    async fn launch(&self, port: u16) -> Result<Child, ShellError> {
        let config = self.write_config(port)?;
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.0.dir.join("saka.log"))
            .map_err(|_| ShellError::LocalStartFailed)?;
        let errors = log.try_clone().map_err(|_| ShellError::LocalStartFailed)?;
        let mut command = Command::new(&self.0.program);
        command
            .arg("serve")
            .env_clear()
            .env("SAKA_CONFIG", &config)
            .stdin(Stdio::null())
            .stdout(log)
            .stderr(errors)
            .kill_on_drop(true);
        for name in INHERITED_ENV {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        let mut child = command.spawn().map_err(|e| {
            tracing::warn!(kind = ?e.kind(), "local saka did not spawn");
            if e.kind() == std::io::ErrorKind::NotFound {
                ShellError::SidecarMissing
            } else {
                ShellError::LocalStartFailed
            }
        })?;
        let deadline = tokio::time::Instant::now() + self.0.timing.ready_within;
        loop {
            if let Ok(Some(status)) = child.try_wait() {
                tracing::warn!(code = ?status.code(), "local saka exited while starting");
                return Err(ShellError::LocalStartFailed);
            }
            if self.ready(port).await {
                return Ok(child);
            }
            if tokio::time::Instant::now() >= deadline {
                let _ = child.kill().await;
                return Err(ShellError::LocalNotReady);
            }
            tokio::time::sleep(self.0.timing.poll_every).await;
        }
    }

    fn write_config(&self, port: u16) -> Result<PathBuf, ShellError> {
        let data = self.0.dir.join("data");
        let text = config_text(port, &data)?;
        std::fs::create_dir_all(&data).map_err(|_| ShellError::LocalStartFailed)?;
        let path = self.0.dir.join("config.toml");
        std::fs::write(&path, text).map_err(|_| ShellError::LocalStartFailed)?;
        Ok(path)
    }

    async fn ready(&self, port: u16) -> bool {
        let url = format!("http://127.0.0.1:{port}/readyz");
        match self
            .0
            .http
            .get(url)
            .timeout(Duration::from_secs(1))
            .send()
            .await
        {
            Ok(response) => response.status().is_success(),
            Err(_) => false,
        }
    }

    fn watch_exit(&self) {
        let inner = self.0.clone();
        tauri::async_runtime::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(500)).await;
                let mut slot = inner.child.lock().await;
                let Some(child) = slot.as_mut() else {
                    return;
                };
                if let Ok(Some(status)) = child.try_wait() {
                    tracing::warn!(code = ?status.code(), "local saka exited");
                    *slot = None;
                    inner.phase.send_replace(Phase::Exited);
                    return;
                }
            }
        });
    }

    pub async fn stop(&self) {
        let mut slot = self.0.child.lock().await;
        let Some(mut child) = slot.take() else {
            if self.phase() != Phase::Stopped {
                self.0.phase.send_replace(Phase::Stopped);
            }
            return;
        };
        let pid = child
            .id()
            .and_then(|id| i32::try_from(id).ok())
            .and_then(rustix::process::Pid::from_raw);
        if let Some(pid) = pid {
            let _ = rustix::process::kill_process(pid, rustix::process::Signal::TERM);
        }
        if tokio::time::timeout(self.0.timing.term_grace, child.wait())
            .await
            .is_err()
        {
            let _ = child.kill().await;
        }
        self.0.phase.send_replace(Phase::Stopped);
    }
}

pub fn free_port() -> Option<u16> {
    std::net::TcpListener::bind(("127.0.0.1", 0))
        .ok()
        .and_then(|listener| listener.local_addr().ok())
        .map(|addr| addr.port())
}

pub fn port_is_free(port: u16) -> bool {
    std::net::TcpListener::bind(("127.0.0.1", port)).is_ok()
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    const FAKE: &str = r#"#!/bin/sh
dir=$(dirname "$SAKA_CONFIG")
mode=$(cat "$dir/mode" 2>/dev/null || echo ok)
echo "$SAKA_CONFIG $* $mode" >> "$dir/invocations"
[ "$mode" = die ] && exit 3
exec python3 - "$SAKA_CONFIG" "$mode" <<'PY'
import sys, re, http.server, signal, threading, os
cfg = open(sys.argv[1]).read()
mode = sys.argv[2]
port = int(re.search(r'listen = "127.0.0.1:(\d+)"', cfg).group(1))
if mode == "ignore_term":
    signal.signal(signal.SIGTERM, signal.SIG_IGN)
if mode == "die_soon":
    threading.Timer(1.0, lambda: os._exit(4)).start()
class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        ok = self.path == "/readyz" and mode != "never_ready"
        self.send_response(200 if ok else 503)
        self.end_headers()
        self.wfile.write(b"{}")
    def log_message(self, *args):
        pass
http.server.HTTPServer(("127.0.0.1", port), Handler).serve_forever()
PY
"#;

    struct Rig {
        sidecar: Sidecar,
        dir: PathBuf,
        _temp: tempfile::TempDir,
    }

    fn rig(mode: &str, program: Option<PathBuf>) -> Rig {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("local");
        std::fs::create_dir_all(&dir).unwrap();
        let script = temp.path().join("saka");
        std::fs::write(&script, FAKE).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(dir.join("mode"), mode).unwrap();
        let timing = Timing {
            ready_within: Duration::from_secs(3),
            poll_every: Duration::from_millis(50),
            term_grace: Duration::from_millis(500),
        };
        let sidecar = Sidecar::new(
            program.unwrap_or(script),
            dir.clone(),
            crate::pairing::client().unwrap(),
            timing,
        );
        Rig {
            sidecar,
            dir,
            _temp: temp,
        }
    }

    async fn answers(port: u16) -> bool {
        reqwest::get(format!("http://127.0.0.1:{port}/readyz"))
            .await
            .is_ok_and(|r| r.status().is_success())
    }

    #[tokio::test]
    async fn start_picks_the_given_port_waits_for_readyz_and_stop_ends_the_process() {
        let rig = rig("ok", None);
        let port = free_port().unwrap();
        rig.sidecar.start(port).await.unwrap();
        assert_eq!(rig.sidecar.phase(), Phase::Running);
        assert!(answers(port).await);
        let config = std::fs::read_to_string(rig.dir.join("config.toml")).unwrap();
        assert!(config.contains(&format!("listen = \"127.0.0.1:{port}\"")));
        assert!(config.contains("allowed_origins = [\"tauri://localhost\"]"));
        assert!(!config.contains("[auth]"));
        let invoked = std::fs::read_to_string(rig.dir.join("invocations")).unwrap();
        assert!(invoked.contains(" serve "), "{invoked}");
        rig.sidecar.stop().await;
        assert_eq!(rig.sidecar.phase(), Phase::Stopped);
        assert!(!answers(port).await);
    }

    #[tokio::test]
    async fn starting_twice_keeps_one_process() {
        let rig = rig("ok", None);
        let port = free_port().unwrap();
        rig.sidecar.start(port).await.unwrap();
        rig.sidecar.start(port).await.unwrap();
        let invoked = std::fs::read_to_string(rig.dir.join("invocations")).unwrap();
        assert_eq!(invoked.lines().count(), 1);
        rig.sidecar.stop().await;
    }

    #[tokio::test]
    async fn a_binary_that_exits_at_once_is_a_start_error() {
        let rig = rig("die", None);
        let error = rig.sidecar.start(free_port().unwrap()).await.unwrap_err();
        assert_eq!(error, ShellError::LocalStartFailed);
        assert_eq!(rig.sidecar.phase(), Phase::Exited);
    }

    #[tokio::test]
    async fn a_binary_that_never_gets_ready_is_stopped_and_reported() {
        let rig = rig("never_ready", None);
        let error = rig.sidecar.start(free_port().unwrap()).await.unwrap_err();
        assert_eq!(error, ShellError::LocalNotReady);
    }

    #[tokio::test]
    async fn a_missing_binary_is_reported_as_missing() {
        let rig = rig("ok", Some(PathBuf::from("/nonexistent/saka")));
        let error = rig.sidecar.start(free_port().unwrap()).await.unwrap_err();
        assert_eq!(error, ShellError::SidecarMissing);
    }

    #[tokio::test]
    async fn a_process_that_dies_later_is_noticed() {
        let rig = rig("die_soon", None);
        rig.sidecar.start(free_port().unwrap()).await.unwrap();
        let mut phases = rig.sidecar.watch();
        tokio::time::timeout(Duration::from_secs(5), async {
            while *phases.borrow_and_update() != Phase::Exited {
                phases.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        assert_eq!(rig.sidecar.phase(), Phase::Exited);
    }

    #[tokio::test]
    async fn a_process_that_ignores_the_stop_signal_is_killed() {
        let rig = rig("ignore_term", None);
        let port = free_port().unwrap();
        rig.sidecar.start(port).await.unwrap();
        rig.sidecar.stop().await;
        assert_eq!(rig.sidecar.phase(), Phase::Stopped);
        assert!(!answers(port).await);
    }

    #[test]
    fn the_config_refuses_a_data_path_the_server_would_reject() {
        assert!(config_text(7000, Path::new("/home/me/Library/Application Support/x")).is_ok());
        assert!(config_text(7000, Path::new("/home/me/(odd)/x")).is_err());
        assert!(config_text(7000, Path::new("/home/\"me\"/x")).is_err());
    }

    #[test]
    fn a_taken_port_is_not_free() {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(!port_is_free(port));
        drop(listener);
        assert!(port_is_free(port));
    }
}
