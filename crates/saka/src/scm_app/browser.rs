use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;
use tokio::task::JoinHandle;

const OPENER: &str = if cfg!(target_os = "macos") {
    "open"
} else {
    "xdg-open"
};
const SETTLE: Duration = Duration::from_secs(2);

pub(crate) struct Opening(JoinHandle<Result<(), String>>);

// The opener runs with the user's own environment: it needs the desktop session
// (display, D-Bus, browser choice), and nothing secret is passed to it.
pub(crate) fn open(url: &str) -> Opening {
    let spawned = Command::new(OPENER)
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    Opening(tokio::spawn(async move {
        let mut child = spawned.map_err(|e| e.kind().to_string())?;
        let status = child.wait().await.map_err(|e| e.kind().to_string())?;
        if status.success() {
            Ok(())
        } else {
            Err(status
                .code()
                .map_or_else(|| "killed".to_string(), |c| format!("exit {c}")))
        }
    }))
}

impl Opening {
    pub(crate) async fn outcome(self) -> Result<(), String> {
        let handle = self.0;
        let abort = handle.abort_handle();
        match tokio::time::timeout(SETTLE, handle).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err("opener_failed".into()),
            Err(_) => {
                abort.abort();
                Ok(())
            }
        }
    }
}
