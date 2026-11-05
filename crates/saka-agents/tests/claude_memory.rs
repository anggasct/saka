#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::print_stdout,
    reason = "an opt-in conformance test against the installed CLI fails loudly"
)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use saka_agents::claude;
use saka_agents::launch::{self, ClaudeSettings, LaunchOptions, LaunchSpec, SessionPaths};
use saka_core::authorship::GitIdentity;

const WAIT: Duration = Duration::from_secs(90);
const PARENT: &str = "parent-memory-7f3a";
const PARENT_DOT: &str = "parent-dot-memory-7f3a";
const ACCOUNT: &str = "account-memory-7f3a";
const WORKTREE: &str = "worktree-memory-7f3a";
const UNAUTHORIZED: &str =
    r#"{"type":"error","error":{"type":"authentication_error","message":"fake"}}"#;

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    account: PathBuf,
}

impl Fixture {
    fn new() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let outer = dir.path().join("outer");
        let root = outer.join("envs").join("e-1");
        let worktree = root.join("worktree");
        let account = dir.path().join("account");
        for d in [
            outer.join(".claude"),
            worktree.clone(),
            root.join("home"),
            root.join("tmp"),
            account.clone(),
        ] {
            std::fs::create_dir_all(d).unwrap();
        }
        let note = |m: &str| format!("Always mention the code word {m}.\n");
        std::fs::write(outer.join("CLAUDE.md"), note(PARENT)).unwrap();
        std::fs::write(outer.join(".claude").join("CLAUDE.md"), note(PARENT_DOT)).unwrap();
        std::fs::write(account.join("CLAUDE.md"), note(ACCOUNT)).unwrap();
        std::fs::write(worktree.join("CLAUDE.md"), note(WORKTREE)).unwrap();
        let git = Command::new("git")
            .args(["init", "-q"])
            .current_dir(&worktree)
            .status()
            .unwrap();
        assert!(git.success());
        Fixture {
            root: std::fs::canonicalize(&root).unwrap(),
            _dir: dir,
            account,
        }
    }

    fn spec(&self) -> LaunchSpec {
        self.spec_with(false)
    }

    fn spec_with(&self, isolated: bool) -> LaunchSpec {
        let settings = ClaudeSettings {
            command: "claude".into(),
            account: "default".into(),
            account_dir: self.account.to_string_lossy().into_owned(),
            model: None,
            path: std::env::var("PATH")
                .unwrap_or_default()
                .split(':')
                .map(String::from)
                .collect(),
            committer: saka(),
        };
        let program = settings
            .program()
            .expect("`claude` is not on PATH; install Claude Code to run this test");
        let paths = SessionPaths::new(
            &self.root.join("run"),
            &self.root.join("sessions"),
            "conformance",
            &self.root,
            &self.root.join("worktree"),
        );
        let options = LaunchOptions {
            cwd: &paths.worktree,
            tools: None,
            model: None,
            resume: None,
            author: &saka(),
            add_dirs: &[],
            isolated,
        };
        launch::claude(&settings, &program, "conformance", &paths, vec![], &options).unwrap()
    }
}

fn saka() -> GitIdentity {
    GitIdentity {
        name: "Saka Agent".into(),
        email: "agent@saka.invalid".into(),
    }
}

fn read_request(stream: &mut TcpStream) -> Option<(String, Vec<u8>)> {
    let mut reader = BufReader::new(stream.try_clone().ok()?);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let target = line
        .split_whitespace()
        .take(2)
        .collect::<Vec<_>>()
        .join(" ");
    let mut length = 0;
    loop {
        let mut header = String::new();
        reader.read_line(&mut header).ok()?;
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body).ok()?;
    Some((target, body))
}

fn fake_api() -> (String, mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let Some((target, body)) = read_request(&mut stream) else {
                continue;
            };
            let (status, reply) = if target.starts_with("POST /v1/messages") {
                let _ = tx.send(String::from_utf8_lossy(&body).into_owned());
                ("401 Unauthorized", UNAUTHORIZED)
            } else {
                ("200 OK", "")
            };
            let _ = write!(
                stream,
                "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{reply}",
                reply.len()
            );
        }
    });
    (url, rx)
}

fn first_request(spec: &LaunchSpec) -> String {
    let (url, requests) = fake_api();
    let mut child = Command::new(&spec.program)
        .args(&spec.args)
        .env_clear()
        .envs(&spec.env)
        .env("ANTHROPIC_BASE_URL", url)
        .env("ANTHROPIC_API_KEY", "sk-ant-conformance-not-a-key")
        .current_dir(&spec.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    for frame in [
        claude::initialize("init-1"),
        claude::user_message("Say hello.", None),
    ] {
        writeln!(stdin, "{frame}").unwrap();
    }
    let body = requests.recv_timeout(WAIT);
    let _ = child.kill();
    let _ = child.wait();
    body.expect("the CLI sent no request to /v1/messages")
}

fn with_settings(spec: &LaunchSpec, settings: &str) -> LaunchSpec {
    let mut spec = spec.clone();
    let at = spec.args.iter().position(|a| a == "--settings").unwrap();
    spec.args[at + 1] = settings.to_string();
    spec
}

#[test]
fn a_session_loads_memory_only_from_its_worktree() {
    if std::env::var("SAKA_TEST_CLAUDE").as_deref() != Ok("1") {
        println!("skipped: set SAKA_TEST_CLAUDE=1, or run `cargo xtask test-claude-memory`");
        return;
    }
    let fx = Fixture::new();
    let spec = fx.spec();
    assert_eq!(Path::new(&spec.cwd), fx.root.join("worktree"));

    let unscoped = first_request(&with_settings(
        &spec,
        r#"{"attribution":{"commit":"","pr":""}}"#,
    ));
    assert!(
        unscoped.contains(PARENT) && unscoped.contains(PARENT_DOT),
        "without claudeMdExcludes the CLI no longer loads CLAUDE.md from parent directories; \
         this fixture no longer proves the scope"
    );

    let scoped = first_request(&spec);
    assert!(
        scoped.contains(WORKTREE),
        "the worktree CLAUDE.md was not loaded"
    );
    for marker in [PARENT, PARENT_DOT, ACCOUNT] {
        assert!(!scoped.contains(marker), "{marker} reached the session");
    }
}

#[test]
fn a_reviewer_session_runs_no_repository_hook_and_reads_no_repository_memory() {
    if std::env::var("SAKA_TEST_CLAUDE").as_deref() != Ok("1") {
        println!("skipped: set SAKA_TEST_CLAUDE=1, or run `cargo xtask test-claude-memory`");
        return;
    }
    let fx = Fixture::new();
    let worktree = fx.root.join("worktree");
    let marker = fx.root.join("hook-ran");
    let hook = serde_json::json!([{ "hooks": [{
        "type": "command",
        "command": format!("touch {}", marker.display()),
    }] }]);
    std::fs::create_dir_all(worktree.join(".claude")).unwrap();
    std::fs::write(
        worktree.join(".claude").join("settings.json"),
        serde_json::json!({ "hooks": { "SessionStart": hook, "UserPromptSubmit": hook } })
            .to_string(),
    )
    .unwrap();

    let control = first_request(&fx.spec_with(false));
    assert!(
        marker.exists() && control.contains(WORKTREE),
        "the control launch must run the repository hook and read its memory, or this fixture proves nothing"
    );
    std::fs::remove_file(&marker).unwrap();

    let reviewer = first_request(&fx.spec_with(true));
    assert!(
        !marker.exists(),
        "a repository hook ran in a reviewer session"
    );
    assert!(
        !reviewer.contains(WORKTREE),
        "repository memory reached a reviewer session"
    );
}
