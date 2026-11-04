#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests fail loudly on a broken setup"
)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use saka_agents::account;
use saka_agents::launch::ClaudeSettings;
use saka_core::authorship::GitIdentity;
use saka_core::diagnostics::{AgentLogin, FoundCli};

struct FakeCli {
    _dir: tempfile::TempDir,
    bin: PathBuf,
    account: PathBuf,
}

impl FakeCli {
    fn new() -> FakeCli {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake-claude");
        std::fs::copy(fixture, bin.join("claude")).unwrap();
        FakeCli {
            account: dir.path().join("account"),
            bin,
            _dir: dir,
        }
    }

    fn settings(&self) -> ClaudeSettings {
        ClaudeSettings {
            command: "claude".into(),
            account: "default".into(),
            account_dir: self.account.to_string_lossy().into_owned(),
            model: None,
            path: vec![
                self.bin.to_string_lossy().into_owned(),
                "/usr/bin".into(),
                "/bin".into(),
            ],
            committer: GitIdentity {
                name: "Saka Agent".into(),
                email: "agent@saka.invalid".into(),
            },
        }
    }

    fn status(&self, mode: &str) {
        std::fs::write(self.bin.join("status"), mode).unwrap();
    }

    fn calls(&self) -> Vec<(String, BTreeMap<String, String>)> {
        let text = std::fs::read_to_string(self.bin.join("calls")).unwrap_or_default();
        let mut calls = Vec::new();
        let mut argv = String::new();
        let mut env = BTreeMap::new();
        for line in text.lines() {
            if let Some(a) = line.strip_prefix("argv ") {
                argv = a.to_string();
            } else if let Some(kv) = line.strip_prefix("env ") {
                let (k, v) = kv.split_once('=').unwrap();
                env.insert(k.to_string(), v.to_string());
            } else if line == "end" {
                calls.push((std::mem::take(&mut argv), std::mem::take(&mut env)));
            }
        }
        calls
    }
}

fn block_on<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(f)
}

#[test]
fn the_login_status_follows_the_exit_code_and_logged_in_field() {
    let cli = FakeCli::new();
    let s = cli.settings();
    for (mode, expected) in [
        (
            "in",
            AgentLogin::LoggedIn {
                auth_method: "claude.ai".into(),
            },
        ),
        ("out", AgentLogin::NotLoggedIn),
        ("exit3", AgentLogin::Unavailable { exit_code: Some(3) }),
        ("garbage", AgentLogin::Unavailable { exit_code: Some(0) }),
    ] {
        cli.status(mode);
        let got = block_on(account::login(&s, account::STATUS_TIMEOUT));
        assert_eq!(got, expected, "{mode}");
        let shown = format!("{got:?}");
        for private in ["operator@example.com", "org-0042", "Acme Secret Org"] {
            assert!(!shown.contains(private), "{mode}: {shown}");
        }
    }
}

#[test]
fn a_status_that_does_not_answer_in_time_is_unavailable() {
    assert_eq!(account::STATUS_TIMEOUT, Duration::from_secs(20));
    let cli = FakeCli::new();
    cli.status("hang");
    let started = Instant::now();
    let got = block_on(account::login(&cli.settings(), Duration::from_millis(300)));
    assert_eq!(got, AgentLogin::Unavailable { exit_code: None });
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn the_status_runs_with_only_path_home_and_the_account_dir() {
    let cli = FakeCli::new();
    cli.status("in");
    let s = cli.settings();
    block_on(account::login(&s, account::STATUS_TIMEOUT));
    let calls = cli.calls();
    assert_eq!(calls.len(), 1);
    let (argv, env) = &calls[0];
    assert_eq!(argv, "auth status --json");
    let account = s.account_dir.clone();
    assert_eq!(
        env.clone(),
        BTreeMap::from([
            ("PATH".to_string(), s.path.join(":")),
            ("HOME".to_string(), account.clone()),
            ("CLAUDE_CONFIG_DIR".to_string(), account),
        ])
    );
}

#[test]
fn the_cli_is_resolved_in_the_agent_path_with_its_version() {
    let cli = FakeCli::new();
    let s = cli.settings();
    let found = block_on(account::cli(&s));
    assert_eq!(
        found.found,
        Some(FoundCli {
            path: cli.bin.join("claude").to_string_lossy().into_owned(),
            version: Some("2.1.7".into()),
        })
    );
    assert_eq!(found.path_dirs, s.path);
    let mut missing = s.clone();
    missing.command = "no-such-cli".into();
    assert_eq!(block_on(account::cli(&missing)).found, None);
    assert_eq!(
        block_on(account::login(&missing, account::STATUS_TIMEOUT)),
        AgentLogin::CliNotFound {
            command: "no-such-cli".into()
        }
    );
    let mut absolute = s;
    absolute.command = cli.bin.join("absent").to_string_lossy().into_owned();
    assert_eq!(block_on(account::cli(&absolute)).found, None);
}

#[test]
fn an_empty_account_dir_reads_as_not_logged_in() {
    let cli = FakeCli::new();
    assert!(!cli.account.exists());
    assert_eq!(
        block_on(account::login(&cli.settings(), account::STATUS_TIMEOUT)),
        AgentLogin::NotLoggedIn
    );
}
