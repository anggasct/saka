use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use base64::Engine;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

const API: &str = "https://api.github.com";
const API_VERSION: &str = "2026-03-10";
const JWT_BACKDATE_SECS: i64 = 60;
const JWT_LIFETIME_SECS: i64 = 540;

pub struct GitHub {
    http: reqwest::Client,
    runtime: tokio::runtime::Runtime,
    repo: String,
    token: String,
}

pub struct PullRequest {
    pub number: u64,
    pub head: String,
    pub merged: bool,
}

pub struct PullState {
    pub open: bool,
    pub merged: bool,
    pub head_sha: String,
}

pub struct IssueView {
    pub open: bool,
    pub state_reason: String,
    pub closed_by: String,
    pub closed_by_bot: bool,
}

pub struct IssueComment {
    pub body: String,
    pub bot: bool,
}

pub struct Review {
    pub commit_id: String,
    pub state: String,
    pub bot: bool,
}

fn jwt(app_id: u64, key_file: &Path) -> Result<String> {
    let pem = std::fs::read(key_file).with_context(|| format!("read {}", key_file.display()))?;
    let key = EncodingKey::from_rsa_pem(&pem).context("the private key is not an RSA PEM key")?;
    let now = jiff::Timestamp::now().as_second();
    let claims = json!({
        "iat": now - JWT_BACKDATE_SECS,
        "exp": now + JWT_LIFETIME_SECS,
        "iss": app_id.to_string(),
    });
    jsonwebtoken::encode(&Header::new(Algorithm::RS256), &claims, &key).context("sign the JWT")
}

impl GitHub {
    pub fn connect(app_id: u64, key_file: &Path, repo: &str) -> Result<GitHub> {
        let mut github = GitHub::with_token(String::new(), repo)?;
        let app = jwt(app_id, key_file)?;
        let installation = github.send_as(
            &app,
            Method::GET,
            &format!("/repos/{repo}/installation"),
            None,
        )?;
        let id = installation["id"]
            .as_u64()
            .with_context(|| format!("the GitHub App is not installed on {repo}"))?;
        let name = repo
            .split('/')
            .nth(1)
            .context("the repo is not <owner>/<name>")?;
        let token = github.send_as(
            &app,
            Method::POST,
            &format!("/app/installations/{id}/access_tokens"),
            Some(json!({ "repositories": [name] })),
        )?;
        github.token = token["token"]
            .as_str()
            .context("no installation token in the reply")?
            .to_string();
        Ok(github)
    }

    pub fn with_token(token: String, repo: &str) -> Result<GitHub> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("start the runtime")?;
        let http = reqwest::Client::builder()
            .user_agent("saka-e2e")
            .timeout(Duration::from_secs(30))
            .build()
            .context("build the HTTP client")?;
        Ok(GitHub {
            http,
            runtime,
            repo: repo.to_string(),
            token,
        })
    }

    fn send_as(
        &self,
        bearer: &str,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Value> {
        let (status, value) = self.raw(bearer, method.clone(), path, body)?;
        if !status.is_success() {
            bail!("{method} {path}: {status} {value}");
        }
        Ok(value)
    }

    fn raw(
        &self,
        bearer: &str,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<(StatusCode, Value)> {
        // A dropped connection fails a whole 30-minute run, so reads and deletes retry (a repeated
        // delete is harmless); other writes do not, because they may have landed before the drop.
        let attempts = if method == Method::GET || method == Method::DELETE {
            3
        } else {
            1
        };
        self.runtime.block_on(async {
            let mut attempt = 1;
            loop {
                let mut request = self
                    .http
                    .request(method.clone(), format!("{API}{path}"))
                    .bearer_auth(bearer)
                    .header("Accept", "application/vnd.github+json")
                    .header("X-GitHub-Api-Version", API_VERSION);
                if let Some(body) = &body {
                    request = request.json(body);
                }
                match request.send().await {
                    Ok(response) => {
                        let status = response.status();
                        let value = response.json::<Value>().await.unwrap_or(Value::Null);
                        return Ok((status, value));
                    }
                    Err(err) if attempt < attempts => {
                        eprintln!("  retry  {path}: {err}");
                        tokio::time::sleep(Duration::from_secs(2 * attempt)).await;
                        attempt += 1;
                    }
                    Err(err) => return Err(err).with_context(|| format!("call {path}")),
                }
            }
        })
    }

    fn send(&self, method: Method, path: &str, body: Option<Value>) -> Result<Value> {
        self.send_as(&self.token, method, path, body)
    }

    pub fn default_branch_sha(&self) -> Result<String> {
        let repo = self.send(Method::GET, &format!("/repos/{}", self.repo), None)?;
        let branch = repo["default_branch"]
            .as_str()
            .context("no default branch")?;
        let head = self.send(
            Method::GET,
            &format!("/repos/{}/git/ref/heads/{branch}", self.repo),
            None,
        )?;
        Ok(head["object"]["sha"]
            .as_str()
            .context("no sha for the default branch")?
            .to_string())
    }

    pub fn create_branch(&self, branch: &str, sha: &str) -> Result<()> {
        self.send(
            Method::POST,
            &format!("/repos/{}/git/refs", self.repo),
            Some(json!({ "ref": format!("refs/heads/{branch}"), "sha": sha })),
        )
        .map(|_| ())
    }

    pub fn put_file(&self, branch: &str, path: &str, content: &str, message: &str) -> Result<()> {
        let encoded = base64::engine::general_purpose::STANDARD.encode(content);
        self.send(
            Method::PUT,
            &format!("/repos/{}/contents/{path}", self.repo),
            Some(json!({ "message": message, "content": encoded, "branch": branch })),
        )
        .map(|_| ())
    }

    pub fn file(&self, branch: &str, path: &str) -> Result<(String, String)> {
        let file = self.send(
            Method::GET,
            &format!("/repos/{}/contents/{path}?ref={branch}", self.repo),
            None,
        )?;
        let encoded: String = file["content"]
            .as_str()
            .with_context(|| format!("no content for {path}"))?
            .split_whitespace()
            .collect();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .with_context(|| format!("decode {path}"))?;
        let sha = file["sha"]
            .as_str()
            .with_context(|| format!("no blob sha for {path}"))?;
        Ok((
            String::from_utf8(bytes).context("the file is not UTF-8")?,
            sha.to_string(),
        ))
    }

    pub fn update_file(
        &self,
        branch: &str,
        path: &str,
        content: &str,
        message: &str,
        blob: &str,
    ) -> Result<()> {
        let encoded = base64::engine::general_purpose::STANDARD.encode(content);
        self.send(
            Method::PUT,
            &format!("/repos/{}/contents/{path}", self.repo),
            Some(json!({ "message": message, "content": encoded, "branch": branch, "sha": blob })),
        )
        .map(|_| ())
    }

    pub fn create_label(&self, name: &str, color: &str, description: &str) -> Result<()> {
        self.send(
            Method::POST,
            &format!("/repos/{}/labels", self.repo),
            Some(json!({ "name": name, "color": color, "description": description })),
        )
        .map(|_| ())
    }

    pub fn delete_label(&self, name: &str) -> Result<()> {
        let (status, value) = self.raw(
            &self.token,
            Method::DELETE,
            &format!("/repos/{}/labels/{name}", self.repo),
            None,
        )?;
        match status {
            StatusCode::NO_CONTENT | StatusCode::NOT_FOUND => Ok(()),
            _ => bail!("delete label {name}: {status} {value}"),
        }
    }

    pub fn create_issue(&self, title: &str, body: &str, labels: &[&str]) -> Result<u64> {
        let issue = self.send(
            Method::POST,
            &format!("/repos/{}/issues", self.repo),
            Some(json!({ "title": title, "body": body, "labels": labels })),
        )?;
        issue["number"]
            .as_u64()
            .context("no issue number in the reply")
    }

    pub fn issue(&self, number: u64) -> Result<IssueView> {
        let issue = self.send(
            Method::GET,
            &format!("/repos/{}/issues/{number}", self.repo),
            None,
        )?;
        Ok(IssueView {
            open: issue["state"] == "open",
            state_reason: issue["state_reason"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            closed_by: issue["closed_by"]["login"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            closed_by_bot: issue["closed_by"]["type"] == "Bot",
        })
    }

    pub fn issue_comments(&self, number: u64) -> Result<Vec<IssueComment>> {
        let list = self.send(
            Method::GET,
            &format!("/repos/{}/issues/{number}/comments?per_page=100", self.repo),
            None,
        )?;
        Ok(list
            .as_array()
            .into_iter()
            .flatten()
            .map(|c| IssueComment {
                body: c["body"].as_str().unwrap_or_default().to_string(),
                bot: c["user"]["type"] == "Bot",
            })
            .collect())
    }

    pub fn comment_on_issue(&self, number: u64, body: &str) -> Result<()> {
        self.send(
            Method::POST,
            &format!("/repos/{}/issues/{number}/comments", self.repo),
            Some(json!({ "body": body })),
        )
        .map(|_| ())
    }

    pub fn close_issue_not_planned(&self, number: u64) -> Result<()> {
        self.send(
            Method::PATCH,
            &format!("/repos/{}/issues/{number}", self.repo),
            Some(json!({ "state": "closed", "state_reason": "not_planned" })),
        )
        .map(|_| ())
    }

    pub fn pull_body(&self, number: u64) -> Result<String> {
        let pull = self.send(
            Method::GET,
            &format!("/repos/{}/pulls/{number}", self.repo),
            None,
        )?;
        Ok(pull["body"].as_str().unwrap_or_default().to_string())
    }

    pub fn branch_sha(&self, branch: &str) -> Result<String> {
        let head = self.send(
            Method::GET,
            &format!("/repos/{}/git/ref/heads/{branch}", self.repo),
            None,
        )?;
        Ok(head["object"]["sha"]
            .as_str()
            .with_context(|| format!("no sha for branch {branch}"))?
            .to_string())
    }

    pub fn open_pull(&self, head: &str, base: &str, title: &str, body: &str) -> Result<u64> {
        let pull = self.send(
            Method::POST,
            &format!("/repos/{}/pulls", self.repo),
            Some(json!({ "title": title, "head": head, "base": base, "body": body })),
        )?;
        pull["number"]
            .as_u64()
            .context("no pull request number in the reply")
    }

    pub fn pull_state(&self, number: u64) -> Result<PullState> {
        let pull = self.send(
            Method::GET,
            &format!("/repos/{}/pulls/{number}", self.repo),
            None,
        )?;
        Ok(PullState {
            open: pull["state"] == "open",
            merged: !pull["merged_at"].is_null(),
            head_sha: pull["head"]["sha"].as_str().unwrap_or_default().to_string(),
        })
    }

    pub fn close_pull(&self, number: u64) -> Result<()> {
        self.send(
            Method::PATCH,
            &format!("/repos/{}/pulls/{number}", self.repo),
            Some(json!({ "state": "closed" })),
        )
        .map(|_| ())
    }

    pub fn checks_done(&self, sha: &str) -> Result<bool> {
        let list = self.send(
            Method::GET,
            &format!("/repos/{}/commits/{sha}/check-runs", self.repo),
            None,
        )?;
        let runs = list["check_runs"].as_array().cloned().unwrap_or_default();
        Ok(!runs.is_empty() && runs.iter().all(|r| r["status"] == "completed"))
    }

    pub fn reviews(&self, number: u64) -> Result<Vec<Review>> {
        let list = self.send(
            Method::GET,
            &format!("/repos/{}/pulls/{number}/reviews?per_page=100", self.repo),
            None,
        )?;
        Ok(list
            .as_array()
            .into_iter()
            .flatten()
            .map(|r| Review {
                commit_id: r["commit_id"].as_str().unwrap_or_default().to_string(),
                state: r["state"].as_str().unwrap_or_default().to_string(),
                bot: r["user"]["type"] == "Bot",
            })
            .collect())
    }

    pub fn branch_exists(&self, branch: &str) -> Result<bool> {
        let (status, value) = self.raw(
            &self.token,
            Method::GET,
            &format!("/repos/{}/git/ref/heads/{branch}", self.repo),
            None,
        )?;
        match status {
            StatusCode::OK => Ok(true),
            StatusCode::NOT_FOUND => Ok(false),
            _ => bail!("look up branch {branch}: {status} {value}"),
        }
    }

    pub fn delete_branch(&self, branch: &str) -> Result<()> {
        let (status, value) = self.raw(
            &self.token,
            Method::DELETE,
            &format!("/repos/{}/git/refs/heads/{branch}", self.repo),
            None,
        )?;
        match status {
            StatusCode::NO_CONTENT | StatusCode::NOT_FOUND | StatusCode::UNPROCESSABLE_ENTITY => {
                Ok(())
            }
            _ => bail!("delete branch {branch}: {status} {value}"),
        }
    }

    pub fn last_commit_touching(&self, branch: &str, path: &str) -> Result<Option<String>> {
        let list = self.send(
            Method::GET,
            &format!(
                "/repos/{}/commits?sha={branch}&path={path}&per_page=1",
                self.repo
            ),
            None,
        )?;
        Ok(list
            .as_array()
            .and_then(|c| c.first())
            .and_then(|c| c["sha"].as_str())
            .map(String::from))
    }

    pub fn pull_requests(&self, base: &str) -> Result<Vec<PullRequest>> {
        let list = self.send(
            Method::GET,
            &format!(
                "/repos/{}/pulls?state=all&base={base}&per_page=100",
                self.repo
            ),
            None,
        )?;
        Ok(list
            .as_array()
            .into_iter()
            .flatten()
            .map(|pr| PullRequest {
                number: pr["number"].as_u64().unwrap_or_default(),
                head: pr["head"]["ref"].as_str().unwrap_or_default().to_string(),
                merged: !pr["merged_at"].is_null(),
            })
            .collect())
    }
}
