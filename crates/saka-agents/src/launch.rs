use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use saka_core::authorship::GitIdentity;
use serde::{Deserialize, Serialize};

pub const HEARTBEAT_MS: u64 = 10_000;
pub const ARCHITECT_TOOLS: &str = "Read,Glob,Grep,LS,TodoWrite,Write,Edit,MultiEdit";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LaunchOptions<'a> {
    pub cwd: &'a Path,
    pub tools: Option<&'a str>,
    pub model: Option<&'a str>,
    pub resume: Option<&'a str>,
    pub author: &'a GitIdentity,
    pub add_dirs: &'a [PathBuf],
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchSpec {
    pub session_id: String,
    pub socket_path: String,
    pub log_path: String,
    pub stderr_path: String,
    pub cwd: String,
    pub program: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub redaction: Redaction,
    #[serde(default)]
    pub secrets_path: Option<String>,
    pub resumed: bool,
    pub heartbeat_ms: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Redaction {
    pub known_values: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionPaths {
    pub socket_path: PathBuf,
    pub session_dir: PathBuf,
    pub env_root: PathBuf,
    pub worktree: PathBuf,
}

impl SessionPaths {
    pub fn new(
        runtime_dir: &Path,
        sessions_dir: &Path,
        session_id: &str,
        env_root: &Path,
        worktree: &Path,
    ) -> SessionPaths {
        SessionPaths {
            socket_path: runtime_dir
                .join("saka")
                .join("runners")
                .join(format!("{session_id}.sock")),
            session_dir: sessions_dir.join(session_id),
            env_root: env_root.to_path_buf(),
            worktree: worktree.to_path_buf(),
        }
    }

    pub fn secrets_file(&self) -> PathBuf {
        self.socket_path.with_extension("secrets.json")
    }

    pub fn launch_file(&self) -> PathBuf {
        self.session_dir.join("launch.json")
    }

    pub fn home(&self) -> PathBuf {
        self.env_root.join("home")
    }

    pub fn tmp(&self) -> PathBuf {
        self.env_root.join("tmp")
    }
}

fn s(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeSettings {
    pub command: String,
    pub account: String,
    pub account_dir: String,
    pub model: Option<String>,
    pub path: Vec<String>,
    pub committer: GitIdentity,
}

impl ClaudeSettings {
    pub fn program(&self) -> Option<PathBuf> {
        if self.command.contains('/') {
            return Some(PathBuf::from(&self.command));
        }
        self.path
            .iter()
            .map(|dir| Path::new(dir).join(&self.command))
            .find(|p| is_executable(p))
    }
}

fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsupportedPath(pub PathBuf);

// Claude Code turns every `\` in a pattern into `/`, so these cannot be escaped in a path.
const GLOB_SPECIAL: &[char] = &[
    '*', '?', '[', ']', '{', '}', '(', ')', '!', '+', '@', '|', '\\',
];

// Claude Code loads CLAUDE.md from every parent of its cwd and matches the physical path.
// It ORs the patterns, so one negated pattern is the only way to keep just this tree.
pub fn memory_scope(dir: &Path) -> Result<String, UnsupportedPath> {
    match dir.to_str() {
        Some(text) if dir.is_absolute() && !text.contains(GLOB_SPECIAL) => {
            Ok(format!("!{}/**", text.trim_end_matches('/')))
        }
        _ => Err(UnsupportedPath(dir.to_path_buf())),
    }
}

fn session_settings(worktree: &Path) -> Result<String, UnsupportedPath> {
    Ok(serde_json::json!({
        // Claude Code's default commit trailer carries `<email>`, which no allow rule can match.
        "attribution": { "commit": "", "pr": "" },
        "claudeMdExcludes": [memory_scope(worktree)?],
    })
    .to_string())
}

pub fn claude(
    settings: &ClaudeSettings,
    program: &Path,
    session_id: &str,
    paths: &SessionPaths,
    known_values: Vec<String>,
    options: &LaunchOptions<'_>,
) -> Result<LaunchSpec, UnsupportedPath> {
    let mut args: Vec<String> = [
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        // Nothing is allowed without an answer, and every prompt comes to the runner over stdio.
        "--permission-mode",
        "manual",
        "--permission-prompt-tool",
        "stdio",
        "--setting-sources",
        "project",
        "--strict-mcp-config",
        "--settings",
    ]
    .map(String::from)
    .to_vec();
    args.push(session_settings(&paths.worktree)?);
    for dir in options.add_dirs {
        args.extend(["--add-dir".to_string(), s(dir)]);
    }
    if let Some(tools) = options.tools {
        args.extend(["--tools".to_string(), tools.to_string()]);
    }
    let model = options
        .model
        .filter(|m| !m.is_empty())
        .or(settings.model.as_deref().filter(|m| !m.is_empty()));
    if let Some(model) = model {
        args.extend(["--model".to_string(), model.to_string()]);
    }
    if let Some(cli_session_id) = options.resume {
        args.extend(["--resume".to_string(), cli_session_id.to_string()]);
    }
    let env: BTreeMap<String, String> = [
        ("PATH", settings.path.join(":")),
        ("HOME", s(&paths.home())),
        ("TMPDIR", s(&paths.tmp())),
        ("LANG", "C.UTF-8".to_string()),
        ("TERM", "dumb".to_string()),
        ("CLAUDE_CONFIG_DIR", settings.account_dir.clone()),
        ("GIT_AUTHOR_NAME", options.author.name.clone()),
        ("GIT_AUTHOR_EMAIL", options.author.email.clone()),
        ("GIT_COMMITTER_NAME", settings.committer.name.clone()),
        ("GIT_COMMITTER_EMAIL", settings.committer.email.clone()),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect();
    Ok(LaunchSpec {
        session_id: session_id.to_string(),
        socket_path: s(&paths.socket_path),
        log_path: s(&paths.session_dir.join("events.ndjson")),
        stderr_path: s(&paths.session_dir.join("cli.stderr")),
        cwd: s(options.cwd),
        program: s(program),
        args,
        env,
        redaction: Redaction { known_values },
        secrets_path: None,
        resumed: options.resume.is_some(),
        heartbeat_ms: HEARTBEAT_MS,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn settings(model: Option<&str>) -> ClaudeSettings {
        ClaudeSettings {
            command: "claude".into(),
            account: "default".into(),
            account_dir: "/data/agents/claude/default".into(),
            model: model.map(String::from),
            path: vec![
                "/home/u/.local/bin".into(),
                "/usr/bin".into(),
                "/bin".into(),
            ],
            committer: saka(),
        }
    }

    fn saka() -> GitIdentity {
        GitIdentity {
            name: "Saka Agent".into(),
            email: "agent@saka.invalid".into(),
        }
    }

    fn octo() -> GitIdentity {
        GitIdentity {
            name: "Octo Cat".into(),
            email: "1+octo@users.noreply.github.com".into(),
        }
    }

    fn paths() -> SessionPaths {
        SessionPaths::new(
            Path::new("/run/user/1000"),
            Path::new("/data/sessions"),
            "s-1",
            Path::new("/data/envs/e-1"),
            Path::new("/data/envs/e-1/worktree"),
        )
    }

    #[test]
    fn the_claude_launch_spec_matches_the_snapshot() {
        let spec = claude(
            &settings(Some("haiku")),
            Path::new("/home/u/.local/bin/claude"),
            "s-1",
            &paths(),
            vec![],
            &LaunchOptions {
                cwd: &paths().worktree,
                tools: None,
                model: None,
                resume: None,
                author: &saka(),
                add_dirs: &[],
            },
        )
        .unwrap();
        assert_eq!(
            serde_json::to_value(&spec).unwrap(),
            json!({
                "session_id": "s-1",
                "socket_path": "/run/user/1000/saka/runners/s-1.sock",
                "log_path": "/data/sessions/s-1/events.ndjson",
                "stderr_path": "/data/sessions/s-1/cli.stderr",
                "cwd": "/data/envs/e-1/worktree",
                "program": "/home/u/.local/bin/claude",
                "args": ["-p", "--input-format", "stream-json", "--output-format", "stream-json", "--verbose",
                         "--permission-mode", "manual", "--permission-prompt-tool", "stdio",
                         "--setting-sources", "project", "--strict-mcp-config",
                         "--settings", "{\"attribution\":{\"commit\":\"\",\"pr\":\"\"},\"claudeMdExcludes\":[\"!/data/envs/e-1/worktree/**\"]}",
                         "--model", "haiku"],
                "env": {
                    "CLAUDE_CONFIG_DIR": "/data/agents/claude/default",
                    "GIT_AUTHOR_EMAIL": "agent@saka.invalid",
                    "GIT_AUTHOR_NAME": "Saka Agent",
                    "GIT_COMMITTER_EMAIL": "agent@saka.invalid",
                    "GIT_COMMITTER_NAME": "Saka Agent",
                    "HOME": "/data/envs/e-1/home",
                    "LANG": "C.UTF-8",
                    "PATH": "/home/u/.local/bin:/usr/bin:/bin",
                    "TERM": "dumb",
                    "TMPDIR": "/data/envs/e-1/tmp",
                },
                "redaction": { "known_values": [] },
                "secrets_path": null,
                "resumed": false,
                "heartbeat_ms": 10000,
            })
        );
    }

    #[test]
    fn a_docs_checkout_is_an_added_dir_and_leaves_the_memory_scope_on_the_worktree() {
        let docs = [PathBuf::from("/data/envs/e-1/docs/docs")];
        let author = saka();
        let launch = |add_dirs: &[PathBuf]| {
            claude(
                &settings(Some("haiku")),
                Path::new("/home/u/.local/bin/claude"),
                "s-1",
                &paths(),
                vec![],
                &LaunchOptions {
                    cwd: Path::new("/data/envs/e-1/worktree"),
                    tools: None,
                    model: None,
                    resume: None,
                    author: &author,
                    add_dirs,
                },
            )
            .unwrap()
        };
        let with = launch(&docs);
        let without = launch(&[]);
        assert_eq!(
            with.args[14..18],
            [
                "{\"attribution\":{\"commit\":\"\",\"pr\":\"\"},\"claudeMdExcludes\":[\"!/data/envs/e-1/worktree/**\"]}",
                "--add-dir",
                "/data/envs/e-1/docs/docs",
                "--model",
            ]
        );
        let mut args = with.args.clone();
        args.drain(15..17);
        assert_eq!(args, without.args);
        assert_eq!(with.env, without.env);
    }

    #[test]
    fn a_user_authored_session_commits_as_the_user_and_saka_stays_the_committer() {
        let spec = claude(
            &settings(None),
            Path::new("/usr/bin/claude"),
            "s-1",
            &paths(),
            vec![],
            &LaunchOptions {
                cwd: &paths().worktree,
                tools: None,
                model: None,
                resume: None,
                author: &octo(),
                add_dirs: &[],
            },
        )
        .unwrap();
        let git: BTreeMap<&str, &str> = spec
            .env
            .iter()
            .filter(|(k, _)| k.starts_with("GIT_"))
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        assert_eq!(
            git,
            BTreeMap::from([
                ("GIT_AUTHOR_EMAIL", "1+octo@users.noreply.github.com"),
                ("GIT_AUTHOR_NAME", "Octo Cat"),
                ("GIT_COMMITTER_EMAIL", "agent@saka.invalid"),
                ("GIT_COMMITTER_NAME", "Saka Agent"),
            ])
        );
        let launch = serde_json::to_string(&spec).unwrap();
        for marker in ["ghu_", "ghr_", "github_pat_", "TOKEN"] {
            assert!(!launch.contains(marker), "{marker} in the launch spec");
        }
    }

    #[test]
    fn a_resumed_session_continues_the_cli_session() {
        let spec = claude(
            &settings(None),
            Path::new("/usr/bin/claude"),
            "s-2",
            &paths(),
            vec![],
            &LaunchOptions {
                cwd: &paths().worktree,
                tools: None,
                model: None,
                resume: Some("cli-abc"),
                author: &saka(),
                add_dirs: &[],
            },
        )
        .unwrap();
        assert_eq!(
            spec.args[spec.args.len() - 2..],
            ["--resume".to_string(), "cli-abc".to_string()]
        );
        assert!(spec.resumed);
    }

    #[test]
    fn the_environment_holds_only_the_allowlist_and_no_credentials() {
        for model in [None, Some(""), Some("sonnet")] {
            let spec = claude(
                &settings(model),
                Path::new("/usr/bin/claude"),
                "s-1",
                &paths(),
                vec![],
                &LaunchOptions {
                    cwd: &paths().worktree,
                    tools: None,
                    model: None,
                    resume: None,
                    author: &octo(),
                    add_dirs: &[],
                },
            )
            .unwrap();
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
            for key in &keys {
                assert!(
                    !key.contains("TOKEN")
                        && !key.contains("KEY")
                        && !key.starts_with("GH_")
                        && !key.starts_with("GITHUB_")
                );
            }
            assert_eq!(
                spec.args.contains(&"--model".to_string()),
                model == Some("sonnet")
            );
        }
    }

    #[test]
    fn the_program_is_found_in_the_agent_path() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("claude");
        std::fs::write(&bin, "#!/bin/sh\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut s = settings(None);
        s.path = vec![
            "/nonexistent".into(),
            dir.path().to_string_lossy().into_owned(),
        ];
        assert_eq!(s.program(), Some(bin));
        s.command = "missing-cli".into();
        assert_eq!(s.program(), None);
    }

    #[test]
    fn the_memory_scope_keeps_only_the_given_tree() {
        for dir in [
            "/data/envs/e-1/worktree",
            "/data/envs/e-1/worktree/",
            "/home/u/my data/saka.v2/env_1-x/worktree",
            "/home/ü/データ/worktree",
        ] {
            let pattern = memory_scope(Path::new(dir)).unwrap();
            assert_eq!(pattern, format!("!{}/**", dir.trim_end_matches('/')));
        }
    }

    #[test]
    fn a_path_the_cli_would_read_as_a_glob_has_no_memory_scope() {
        for c in GLOB_SPECIAL {
            let dir = PathBuf::from(format!("/data/en{c}vs/e-1/worktree"));
            assert_eq!(memory_scope(&dir), Err(UnsupportedPath(dir.clone())), "{c}");
        }
        assert!(memory_scope(Path::new("envs/e-1/worktree")).is_err());
        assert_eq!(GLOB_SPECIAL.len(), 13);
    }

    #[test]
    fn a_launch_in_an_unsupported_path_is_refused() {
        let paths = SessionPaths::new(
            Path::new("/run/user/1000"),
            Path::new("/data/sessions"),
            "s-1",
            Path::new("/data/[envs]/e-1"),
            Path::new("/data/[envs]/e-1/worktree"),
        );
        assert_eq!(
            claude(
                &settings(None),
                Path::new("/usr/bin/claude"),
                "s-1",
                &paths,
                vec![],
                &LaunchOptions {
                    cwd: &paths.worktree,
                    tools: None,
                    model: None,
                    resume: None,
                    author: &saka(),
                    add_dirs: &[],
                },
            ),
            Err(UnsupportedPath(PathBuf::from("/data/[envs]/e-1/worktree")))
        );
    }
}
