use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::protocol::PROTOCOL_VERSION;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchSpec {
    pub session_id: String,
    pub protocol_version: u32,
    pub socket_path: String,
    pub log_path: String,
    pub stderr_path: String,
    pub cwd: String,
    pub program: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub redaction: Redaction,
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
    pub account_dir: String,
    pub model: Option<String>,
    pub path: Vec<String>,
    pub author_name: String,
    pub author_email: String,
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

// Claude Code's default commit trailer carries `<email>`, which no allow rule can match.
const NO_ATTRIBUTION: &str = r#"{"attribution":{"commit":"","pr":""}}"#;

pub fn claude(
    settings: &ClaudeSettings,
    program: &Path,
    session_id: &str,
    paths: &SessionPaths,
    known_values: Vec<String>,
) -> LaunchSpec {
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
        NO_ATTRIBUTION,
    ]
    .map(String::from)
    .to_vec();
    if let Some(model) = settings.model.as_ref().filter(|m| !m.is_empty()) {
        args.extend(["--model".to_string(), model.clone()]);
    }
    let env: BTreeMap<String, String> = [
        ("PATH", settings.path.join(":")),
        ("HOME", s(&paths.home())),
        ("TMPDIR", s(&paths.tmp())),
        ("LANG", "C.UTF-8".to_string()),
        ("TERM", "dumb".to_string()),
        ("CLAUDE_CONFIG_DIR", settings.account_dir.clone()),
        ("GIT_AUTHOR_NAME", settings.author_name.clone()),
        ("GIT_AUTHOR_EMAIL", settings.author_email.clone()),
        ("GIT_COMMITTER_NAME", settings.author_name.clone()),
        ("GIT_COMMITTER_EMAIL", settings.author_email.clone()),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect();
    LaunchSpec {
        session_id: session_id.to_string(),
        protocol_version: PROTOCOL_VERSION,
        socket_path: s(&paths.socket_path),
        log_path: s(&paths.session_dir.join("events.ndjson")),
        stderr_path: s(&paths.session_dir.join("cli.stderr")),
        cwd: s(&paths.worktree),
        program: s(program),
        args,
        env,
        redaction: Redaction { known_values },
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn settings(model: Option<&str>) -> ClaudeSettings {
        ClaudeSettings {
            command: "claude".into(),
            account_dir: "/data/agents/claude/default".into(),
            model: model.map(String::from),
            path: vec![
                "/home/u/.local/bin".into(),
                "/usr/bin".into(),
                "/bin".into(),
            ],
            author_name: "Saka Agent".into(),
            author_email: "agent@saka.invalid".into(),
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
        );
        assert_eq!(
            serde_json::to_value(&spec).unwrap(),
            json!({
                "session_id": "s-1",
                "protocol_version": 1,
                "socket_path": "/run/user/1000/saka/runners/s-1.sock",
                "log_path": "/data/sessions/s-1/events.ndjson",
                "stderr_path": "/data/sessions/s-1/cli.stderr",
                "cwd": "/data/envs/e-1/worktree",
                "program": "/home/u/.local/bin/claude",
                "args": ["-p", "--input-format", "stream-json", "--output-format", "stream-json", "--verbose",
                         "--permission-mode", "manual", "--permission-prompt-tool", "stdio",
                         "--setting-sources", "project", "--strict-mcp-config",
                         "--settings", "{\"attribution\":{\"commit\":\"\",\"pr\":\"\"}}",
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
            })
        );
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
            );
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
}
