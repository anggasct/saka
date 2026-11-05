use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use saka_core::redact::Redactor;
use saka_ports::{
    CommandUnit, HostCommand, HostCommands, HostOutcome, PortError, PortResult, Supervisor,
    UnitLimits, output_tail,
};
use serde_json::json;

pub struct HostRunner {
    pub supervisor: Arc<dyn Supervisor>,
    pub limits: UnitLimits,
    pub path: Vec<String>,
    pub scratch_root: PathBuf,
    pub logs_dir: PathBuf,
}

fn io(op: &str, e: &std::io::Error) -> PortError {
    PortError::retryable(
        "host_io_failed",
        json!({ "op": op, "error": e.to_string() }),
    )
}

fn key_ok(key: &str) -> bool {
    !key.is_empty()
        && key.split('/').all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        })
}

fn private_dir(path: &Path) -> std::io::Result<()> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
}

fn write_log(path: &Path, text: &str, truncated: bool) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(dir) = path.parent() {
        private_dir(dir)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    if truncated {
        file.write_all(b"[output truncated: only the last 8 MiB is kept]\n")?;
    }
    file.write_all(text.as_bytes())
}

#[async_trait]
impl HostCommands for HostRunner {
    async fn run(&self, command: &HostCommand) -> PortResult<HostOutcome> {
        if !key_ok(&command.scratch_key) || !key_ok(&command.name) {
            return Err(PortError::permanent("host_name_invalid", json!({})));
        }
        let scratch = self.scratch_root.join(&command.scratch_key);
        let home = scratch.join("home");
        private_dir(&home).map_err(|e| io("create_scratch", &e))?;
        let cwd = match &command.cwd {
            Some(dir) if Path::new(dir).is_dir() => dir.clone(),
            Some(dir) => {
                return Err(PortError::permanent(
                    "host_cwd_missing",
                    json!({ "cwd": dir }),
                ));
            }
            None => scratch.to_string_lossy().into_owned(),
        };
        let mut env = std::collections::BTreeMap::new();
        env.insert("PATH".to_string(), self.path.join(":"));
        env.insert("HOME".to_string(), home.to_string_lossy().into_owned());
        env.insert("LANG".to_string(), "C.UTF-8".to_string());
        env.extend(
            command
                .env
                .iter()
                .filter(|(k, _)| k.starts_with("SAKA_"))
                .map(|(k, v)| (k.clone(), v.clone())),
        );
        let unit = CommandUnit {
            name: format!("saka-hook-{}", command.name.replace('/', "-")),
            program: "/bin/sh".into(),
            args: vec!["-c".into(), command.command.clone()],
            cwd,
            env,
            secret_env: Vec::new(),
            limits: self.limits.clone(),
            timeout: Duration::from_millis(command.timeout_ms),
        };
        let started = Instant::now();
        let outcome = self.supervisor.run(&unit).await?;
        let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let text = String::from_utf8_lossy(&outcome.output);
        let redacted = Redactor::new(&[]).text(&text).into_owned();
        let log = self
            .logs_dir
            .join(format!("{}.log", command.name.replace('/', "-")));
        write_log(&log, &redacted, outcome.truncated).map_err(|e| io("write_hook_log", &e))?;
        Ok(HostOutcome {
            exit_code: outcome.exit_code,
            timed_out: outcome.timed_out,
            duration_ms,
            log_path: log.to_string_lossy().into_owned(),
            output_tail: output_tail(&redacted),
        })
    }
}
