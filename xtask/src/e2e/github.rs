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
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("start the runtime")?;
        let http = reqwest::Client::builder()
            .user_agent("saka-e2e")
            .timeout(Duration::from_secs(30))
            .build()
            .context("build the HTTP client")?;
        let mut github = GitHub {
            http,
            runtime,
            repo: repo.to_string(),
            token: String::new(),
        };
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
        let mut request = self
            .http
            .request(method, format!("{API}{path}"))
            .bearer_auth(bearer)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", API_VERSION);
        if let Some(body) = body {
            request = request.json(&body);
        }
        self.runtime.block_on(async {
            let response = request
                .send()
                .await
                .with_context(|| format!("call {path}"))?;
            let status = response.status();
            let value = response.json::<Value>().await.unwrap_or(Value::Null);
            Ok((status, value))
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
