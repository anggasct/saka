use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde_json::json;
use tokio::process::Command;

use super::{EXIT_USAGE, InitError};

const GIT_TIMEOUT: Duration = Duration::from_secs(10);

pub struct Reply {
    pub ok: bool,
    pub stdout: Vec<u8>,
}

fn command(dir: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.env_clear();
    for name in ["PATH", "HOME"] {
        if let Some(value) = std::env::var_os(name) {
            cmd.env(name, value);
        }
    }
    cmd.env("LANG", "C")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .arg("-C")
        .arg(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    cmd
}

fn git_failed(reason: String) -> InitError {
    InitError::failed("git_failed", json!({ "reason": reason }))
}

async fn run(dir: &Path, args: &[&str]) -> Result<Reply, InitError> {
    let mut cmd = command(dir);
    cmd.args(args);
    let output = tokio::time::timeout(GIT_TIMEOUT, cmd.output())
        .await
        .map_err(|_| git_failed(format!("git {} timed out", args.join(" "))))?
        .map_err(|e| git_failed(e.to_string()))?;
    Ok(Reply {
        ok: output.status.success(),
        stdout: output.stdout,
    })
}

fn line(reply: &Reply) -> Option<String> {
    let text = String::from_utf8(reply.stdout.clone()).ok()?;
    let line = text.trim_end_matches(['\n', '\r']).to_string();
    (reply.ok && !line.is_empty()).then_some(line)
}

pub async fn toplevel(dir: &Path) -> Result<PathBuf, InitError> {
    let reply = run(dir, &["rev-parse", "--show-toplevel"]).await?;
    line(&reply).map(PathBuf::from).ok_or_else(|| {
        InitError::usage(
            "not_a_git_repo",
            json!({ "path": dir.display().to_string() }),
        )
    })
}

pub async fn remote_url(root: &Path, remote: &str) -> Result<String, InitError> {
    let reply = run(root, &["remote", "get-url", remote]).await?;
    line(&reply).ok_or_else(|| InitError::usage("remote_missing", json!({ "remote": remote })))
}

pub async fn default_branch(root: &Path, remote: &str) -> Result<Option<String>, InitError> {
    let head = format!("refs/remotes/{remote}/HEAD");
    let reply = run(root, &["symbolic-ref", "--quiet", "--short", &head]).await?;
    let prefix = format!("{remote}/");
    Ok(line(&reply).map(|b| b.strip_prefix(&prefix).map(String::from).unwrap_or(b)))
}

pub async fn pending(root: &Path, paths: &[String]) -> Result<Vec<String>, InitError> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }
    let mut args = vec![
        "status",
        "--porcelain=v1",
        "-z",
        "--untracked-files=all",
        "--",
    ];
    args.extend(paths.iter().map(String::as_str));
    let reply = run(root, &args).await?;
    if !reply.ok {
        return Err(git_failed("git status failed".into()));
    }
    let mut out = Vec::new();
    let mut entries = reply.stdout.split(|b| *b == 0);
    while let Some(entry) = entries.next() {
        let Some(path) = entry.get(3..).filter(|p| !p.is_empty()) else {
            continue;
        };
        if entry.first() == Some(&b'R') || entry.first() == Some(&b'C') {
            entries.next();
        }
        out.push(String::from_utf8_lossy(path).into_owned());
    }
    out.sort();
    out.dedup();
    Ok(out)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteRepo {
    pub url: String,
    pub name: String,
}

fn plain(segment: &str) -> bool {
    !segment.is_empty()
        && segment != "."
        && segment != ".."
        && segment
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

fn host_ok(host: &str) -> bool {
    !host.is_empty()
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-'))
}

pub fn normalize(url: &str) -> Option<RemoteRepo> {
    let url = url.trim();
    let (host, path) = if let Some(rest) = url.strip_prefix("https://") {
        let (authority, path) = rest.split_once('/')?;
        let host = authority.rsplit('@').next()?;
        (host, path)
    } else if let Some(rest) = url.strip_prefix("ssh://") {
        let (authority, path) = rest.split_once('/')?;
        let host = authority.rsplit('@').next()?;
        let host = match host.split_once(':') {
            Some((h, port)) if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => h,
            Some(_) => return None,
            None => host,
        };
        (host, path)
    } else if !url.contains("://") {
        let (authority, path) = url.split_once(':')?;
        let (user, host) = authority.rsplit_once('@')?;
        if user.is_empty() || path.starts_with('/') {
            return None;
        }
        (host, path)
    } else {
        return None;
    };
    let path = path.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let mut segments = path.split('/');
    let (owner, name) = (segments.next()?, segments.next()?);
    if segments.next().is_some() || !host_ok(host) || !plain(owner) || !plain(name) {
        return None;
    }
    Some(RemoteRepo {
        url: format!("https://{}/{owner}/{name}", host.to_ascii_lowercase()),
        name: name.to_string(),
    })
}

pub async fn discover(
    dir: &Path,
    remote: &str,
) -> Result<(PathBuf, RemoteRepo, Option<String>), InitError> {
    let root = toplevel(dir).await?;
    let raw = remote_url(&root, remote).await?;
    let repo = normalize(&raw)
        .ok_or_else(|| InitError::new(EXIT_USAGE, "remote_unsupported", json!({ "url": raw })))?;
    let branch = default_branch(&root, remote).await?;
    Ok((root, repo, branch))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_urls_normalize_to_https_owner_and_name() {
        for (raw, want) in [
            (
                "https://github.com/acme/shop",
                "https://github.com/acme/shop",
            ),
            (
                "https://user@GitHub.com/acme/shop.git",
                "https://github.com/acme/shop",
            ),
            (
                "https://x:token@github.com/acme/shop/",
                "https://github.com/acme/shop",
            ),
            (
                "ssh://git@github.com:2222/acme/shop.git",
                "https://github.com/acme/shop",
            ),
            ("ssh://github.com/Acme/Shop", "https://github.com/Acme/Shop"),
            (
                "git@GITHUB.com:acme/shop.git",
                "https://github.com/acme/shop",
            ),
            (
                "git@ghe.acme.test:team/app",
                "https://ghe.acme.test/team/app",
            ),
        ] {
            assert_eq!(
                normalize(raw).map(|r| r.url).as_deref(),
                Some(want),
                "{raw}"
            );
        }
        assert_eq!(
            normalize("git@github.com:acme/shop.git").unwrap().name,
            "shop"
        );
        for bad in [
            "file:///srv/git/shop.git",
            "/srv/git/shop.git",
            "../shop",
            "https://github.com/acme/shop/extra",
            "https://github.com/acme",
            "http://github.com/acme/shop",
            "git@github.com:/abs/shop",
            "ssh://git@github.com:x/acme/shop",
            "github.com:acme/shop",
            "https://github.com/../shop",
        ] {
            assert_eq!(normalize(bad), None, "{bad}");
        }
    }
}
