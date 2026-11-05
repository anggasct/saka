#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration tests assert by panicking"
)]

use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;

use saka_env::{DirectSupervisor, HostRunner};
use saka_ports::{HostCommand, HostCommands, UnitLimits};

fn runner(dir: &std::path::Path) -> HostRunner {
    HostRunner {
        supervisor: Arc::new(DirectSupervisor::new(dir.join("units"))),
        limits: UnitLimits {
            memory_max: "1G".into(),
            cpu_quota: "100%".into(),
            tasks_max: 64,
        },
        path: vec!["/usr/bin".into(), "/bin".into()],
        scratch_root: dir.join("hooks"),
        logs_dir: dir.join("logs"),
    }
}

fn command(text: &str) -> HostCommand {
    HostCommand {
        name: "run-1".into(),
        command: text.into(),
        cwd: None,
        env: BTreeMap::from([
            ("SAKA_EVENT".to_string(), "merged".to_string()),
            ("SAKA_SHA".to_string(), "abc".to_string()),
            ("DEPLOY_TOKEN".to_string(), "must-not-pass".to_string()),
            ("PATH".to_string(), "/evil".to_string()),
        ]),
        scratch_key: "project-1/deploy".into(),
        timeout_ms: 5_000,
    }
}

#[tokio::test]
async fn the_command_sees_only_the_allowlisted_environment() {
    let dir = tempfile::tempdir().unwrap();
    let out = runner(dir.path()).run(&command("env")).await.unwrap();
    assert_eq!(out.exit_code, Some(0));
    let log = std::fs::read_to_string(&out.log_path).unwrap();
    let keys: BTreeSet<&str> = log
        .lines()
        .filter_map(|l| l.split_once('=').map(|(k, _)| k))
        .filter(|k| !matches!(*k, "PWD" | "OLDPWD" | "SHLVL" | "_"))
        .collect();
    assert_eq!(
        keys,
        BTreeSet::from(["HOME", "LANG", "PATH", "SAKA_EVENT", "SAKA_SHA"])
    );
    assert!(log.contains("PATH=/usr/bin:/bin"));
    assert!(!log.contains("must-not-pass"));
    assert!(!log.contains("/evil"));
    assert!(log.contains("SAKA_EVENT=merged"));
}

#[tokio::test]
async fn the_scratch_directory_is_private_and_the_default_working_directory() {
    let dir = tempfile::tempdir().unwrap();
    let out = runner(dir.path()).run(&command("pwd")).await.unwrap();
    let scratch = dir.path().join("hooks/project-1/deploy");
    assert!(
        out.output_tail.trim().ends_with("project-1/deploy"),
        "{}",
        out.output_tail
    );
    for path in [scratch.clone(), scratch.join("home")] {
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "{}", path.display());
    }
    let log_mode = std::fs::metadata(&out.log_path)
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(log_mode, 0o600);
}

#[tokio::test]
async fn a_given_working_directory_must_exist() {
    let dir = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let mut c = command("pwd");
    c.cwd = Some(work.path().to_string_lossy().into_owned());
    let out = runner(dir.path()).run(&c).await.unwrap();
    assert!(
        out.output_tail
            .trim()
            .ends_with(work.path().file_name().unwrap().to_str().unwrap())
    );
    c.cwd = Some("/definitely/not/here".into());
    let err = runner(dir.path()).run(&c).await.unwrap_err();
    assert_eq!(err.code, "host_cwd_missing");
    assert!(!err.retryable);
}

#[tokio::test]
async fn exit_codes_timeouts_and_token_patterns_are_reported() {
    let dir = tempfile::tempdir().unwrap();
    let r = runner(dir.path());
    let failed = r.run(&command("echo boom; exit 7")).await.unwrap();
    assert_eq!(failed.exit_code, Some(7));
    assert!(!failed.timed_out);
    assert!(failed.output_tail.contains("boom"));

    let mut slow = command("sleep 30");
    slow.timeout_ms = 200;
    let out = r.run(&slow).await.unwrap();
    assert!(out.timed_out);
    assert_eq!(out.exit_code, None);

    let leaky = r
        .run(&command(
            "echo token ghp_0123456789abcdefghijklmnopqrstuvwxyz0123",
        ))
        .await
        .unwrap();
    assert!(
        !leaky.output_tail.contains("ghp_0123"),
        "{}",
        leaky.output_tail
    );
    let log = std::fs::read_to_string(&leaky.log_path).unwrap();
    assert!(!log.contains("ghp_0123"));
}

#[tokio::test]
async fn names_that_could_escape_the_scratch_root_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    for bad in ["../x", "a/../b", "/abs", "a//b", ""] {
        let mut c = command("true");
        c.scratch_key = bad.into();
        let err = runner(dir.path()).run(&c).await.unwrap_err();
        assert_eq!(err.code, "host_name_invalid", "{bad:?}");
    }
    let mut c = command("true");
    c.name = "../log".into();
    assert!(runner(dir.path()).run(&c).await.is_err());
}
