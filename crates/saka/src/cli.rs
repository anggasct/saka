use std::io::Write;
use std::process::ExitCode;

use saka_core::secret::SecretText;
use saka_ports::BestEffort;
use serde_json::{Value, json};
use uuid::Uuid;
use zeroize::Zeroizing;

pub const EXIT_PROBLEMS: u8 = 3;
pub const EXIT_CONFLICT: u8 = 4;
pub const EXIT_PENDING: u8 = 5;
const DRAFT_POLL: std::time::Duration = std::time::Duration::from_secs(2);

pub struct NewDraft<'a> {
    pub project: &'a str,
    pub path: &'a str,
    pub idea: &'a str,
    pub context: &'a [String],
    pub locale: Option<&'a str>,
    pub session: bool,
    pub revise: bool,
    pub wait: bool,
}

pub struct NewRule<'a> {
    pub project: &'a str,
    pub effect: &'a str,
    pub tool: &'a str,
    pub match_kind: &'a str,
    pub command: Option<&'a str>,
    pub path: Option<&'a str>,
    pub note: Option<&'a str>,
}

pub struct Client {
    base: String,
    http: reqwest::Client,
    timeout: Option<std::time::Duration>,
    pub json: bool,
}

pub struct Answer {
    pub status: u16,
    pub body: Value,
    pub etag: Option<String>,
}

impl Answer {
    fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

pub(crate) fn out(text: &str) {
    writeln!(std::io::stdout(), "{text}").best_effort("print to stdout");
}

fn out_text(text: &str) {
    out(text);
}

fn write_token(path: &std::path::Path, token: &str) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(token.as_bytes())?;
    file.sync_all()
}

pub(crate) fn err(text: &str) {
    writeln!(std::io::stderr(), "{text}").best_effort("print to stderr");
}

fn issue_line(issue: &Value) -> String {
    let prefix = if issue["severity"] == "warning" {
        "warning "
    } else {
        ""
    };
    format!(
        "{prefix}line {}: {} {}",
        issue["line"],
        text(&issue["code"]),
        issue["params"]
    )
}

fn conflict_line(step: &Value) -> String {
    let fields: Vec<String> = step["fields"]
        .as_array()
        .into_iter()
        .flatten()
        .map(text)
        .collect();
    format!(
        "conflict {}: {} {}",
        text(&step["step_id"]),
        text(&step["reason"]),
        fields.join(",")
    )
    .trim_end()
    .to_string()
}

pub fn read_secret(name: &str, multiline: bool) -> Result<SecretText, ExitCode> {
    use std::io::{BufRead, IsTerminal, Read};
    let stdin = std::io::stdin();
    let typed = stdin.is_terminal();
    let restore = if typed {
        write!(std::io::stderr(), "Value for {name}: ").best_effort("print the prompt");
        std::io::stderr().flush().best_effort("flush the prompt");
        hide_input()
    } else {
        None
    };
    let mut raw = Zeroizing::new(String::new());
    let read = if multiline {
        stdin.lock().read_to_string(&mut raw).map(drop)
    } else {
        stdin.lock().read_line(&mut raw).map(drop)
    };
    if let Some(saved) = restore {
        rustix::termios::tcsetattr(
            std::io::stdin(),
            rustix::termios::OptionalActions::Now,
            &saved,
        )
        .best_effort("restore the terminal");
        err("");
    }
    if let Err(e) = read {
        err(&format!(
            "error: stdin_unreadable {}",
            json!({ "reason": e.to_string() })
        ));
        return Err(ExitCode::FAILURE);
    }
    let value = if multiline {
        raw.as_str()
    } else {
        raw.trim_end_matches('\n').trim_end_matches('\r')
    };
    Ok(SecretText::new(value.to_string()))
}

pub fn read_idea(file: &std::path::Path) -> Result<String, ExitCode> {
    std::fs::read_to_string(file).map_err(|e| {
        err(&format!(
            "error: idea_file_unreadable {}",
            json!({ "path": file.display().to_string(), "reason": e.to_string() })
        ));
        ExitCode::FAILURE
    })
}

fn hide_input() -> Option<rustix::termios::Termios> {
    let stdin = std::io::stdin();
    let saved = rustix::termios::tcgetattr(&stdin).ok()?;
    let mut quiet = saved.clone();
    quiet.local_modes.remove(rustix::termios::LocalModes::ECHO);
    rustix::termios::tcsetattr(&stdin, rustix::termios::OptionalActions::Now, &quiet).ok()?;
    Some(saved)
}

pub fn refuse(code: &str, params: &Value) {
    err(&format!("error: {code} {params}"));
}

fn fail(a: &Answer) -> ExitCode {
    let code = a.body["code"].as_str().unwrap_or("error");
    err(&format!("error: {code} {}", a.body["params"]));
    match a.status {
        409 | 412 => ExitCode::from(EXIT_CONFLICT),
        _ => ExitCode::FAILURE,
    }
}

impl Client {
    pub fn new(base: &str, json: bool) -> Client {
        Client {
            base: base.trim_end_matches('/').to_string(),
            http: reqwest::Client::new(),
            timeout: None,
            json,
        }
    }

    pub fn with_timeout(base: &str, json: bool, timeout: std::time::Duration) -> Client {
        Client::new(base, json).rebuilt(Some(timeout), None)
    }

    pub fn authorized(self, token: Option<&SecretText>) -> Client {
        let timeout = self.timeout;
        self.rebuilt(timeout, token)
    }

    fn rebuilt(
        mut self,
        timeout: Option<std::time::Duration>,
        token: Option<&SecretText>,
    ) -> Client {
        let mut builder = reqwest::Client::builder();
        if let Some(timeout) = timeout {
            builder = builder.timeout(timeout).connect_timeout(timeout);
        }
        if let Some(token) = token {
            let mut headers = reqwest::header::HeaderMap::new();
            if let Ok(mut value) =
                reqwest::header::HeaderValue::from_str(&format!("Bearer {}", token.expose()))
            {
                value.set_sensitive(true);
                headers.insert(reqwest::header::AUTHORIZATION, value);
            }
            builder = builder
                .default_headers(headers)
                .redirect(reqwest::redirect::Policy::none());
        }
        self.http = builder.build().unwrap_or_else(|_| reqwest::Client::new());
        self.timeout = timeout;
        self
    }

    pub async fn get_quiet(&self, path: &str) -> Result<Answer, String> {
        self.exchange(self.http.get(format!("{}{path}", self.base)))
            .await
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    async fn exchange(&self, req: reqwest::RequestBuilder) -> Result<Answer, String> {
        let response = req.send().await.map_err(|e| e.to_string())?;
        let status = response.status().as_u16();
        let etag = response
            .headers()
            .get("etag")
            .and_then(|v| v.to_str().ok())
            .map(String::from);
        let body = response.json::<Value>().await.unwrap_or(Value::Null);
        Ok(Answer { status, body, etag })
    }

    async fn send(&self, req: reqwest::RequestBuilder) -> Result<Answer, ExitCode> {
        self.exchange(req).await.map_err(|reason| {
            err(&format!(
                "error: server_unreachable {}",
                json!({ "server": self.base, "reason": reason })
            ));
            ExitCode::FAILURE
        })
    }

    pub async fn post_quiet(&self, path: &str, body: &Value) -> Result<Answer, String> {
        let req = self
            .http
            .post(format!("{}{path}", self.base))
            .header("Idempotency-Key", Uuid::now_v7().to_string())
            .json(body);
        self.exchange(req).await
    }

    pub async fn get(&self, path: &str) -> Result<Answer, ExitCode> {
        self.send(self.http.get(format!("{}{path}", self.base)))
            .await
    }

    pub async fn post(
        &self,
        path: &str,
        body: &Value,
        if_match: Option<&str>,
    ) -> Result<Answer, ExitCode> {
        let mut req = self
            .http
            .post(format!("{}{path}", self.base))
            .header("Idempotency-Key", Uuid::now_v7().to_string())
            .json(body);
        if let Some(tag) = if_match {
            req = req.header("If-Match", tag);
        }
        self.send(req).await
    }

    fn done(&self, a: &Answer, summary: impl FnOnce(&Value) -> String) -> ExitCode {
        if self.json {
            out(&a.body.to_string());
        } else {
            out(&summary(&a.body));
        }
        ExitCode::SUCCESS
    }

    fn finish(&self, a: &Answer, summary: impl FnOnce(&Value) -> String) -> ExitCode {
        if a.ok() {
            self.done(a, summary)
        } else {
            if self.json {
                out(&a.body.to_string());
            }
            fail(a)
        }
    }

    pub async fn project_add(&self, url: &str, path: &str, base: Option<&str>) -> ExitCode {
        let body = json!({ "workspace_url": url, "project_path": path, "base_branch": base });
        match self.post("/api/v1/projects", &body, None).await {
            Ok(a) => self.finish(&a, |b| {
                format!("project {} {}", text(&b["id"]), text(&b["name"]))
            }),
            Err(code) => code,
        }
    }

    pub async fn project_reload(&self, project: &str) -> ExitCode {
        let id = match self.project_id(project).await {
            Ok(id) => id,
            Err(code) => return code,
        };
        let path = format!("/api/v1/projects/{id}/actions/reload");
        match self.post(&path, &json!({}), None).await {
            Ok(a) if a.ok() => self.done(&a, |b| {
                format!(
                    "project {} {}",
                    text(&b["project"]["name"]),
                    text(&b["outcome"])
                )
            }),
            Ok(a) => {
                if self.json {
                    out(&a.body.to_string());
                }
                fail(&a);
                ExitCode::FAILURE
            }
            Err(code) => code,
        }
    }

    pub async fn project_authorship(&self, project: &str, mode: Option<&str>) -> ExitCode {
        let id = match self.project_id(project).await {
            Ok(id) => id,
            Err(code) => return code,
        };
        let path = format!("/api/v1/projects/{id}/authorship");
        let answer = match mode {
            Some(mode) => {
                let body = json!({ "mode": mode });
                self.keyed(reqwest::Method::PUT, &path, Some(&body)).await
            }
            None => self.get(&path).await,
        };
        match answer {
            Ok(a) => self.finish(&a, |b| {
                let author = match b["author"].as_object() {
                    Some(author) => {
                        format!("{} <{}>", text(&author["name"]), text(&author["email"]))
                    }
                    None => "unavailable".to_string(),
                };
                format!("mode {} author {author}", text(&b["mode"]))
            }),
            Err(code) => code,
        }
    }

    pub async fn user_token_set(&self, project: &str, value: &SecretText) -> ExitCode {
        let id = match self.project_id(project).await {
            Ok(id) => id,
            Err(code) => return code,
        };
        let body = json!({ "value": value.expose() });
        match self
            .keyed(
                reqwest::Method::PUT,
                &format!("/api/v1/projects/{id}/authorship/user-token"),
                Some(&body),
            )
            .await
        {
            Ok(a) => self.finish(&a, |b| {
                let token = &b["user_token"];
                let expires = match token["expires_at"].as_str() {
                    Some(at) => format!("expires {at}"),
                    None => "no expiry".to_string(),
                };
                format!("user token of {} set, {expires}", text(&token["login"]))
            }),
            Err(code) => code,
        }
    }

    pub async fn user_token_rm(&self, project: &str) -> ExitCode {
        let id = match self.project_id(project).await {
            Ok(id) => id,
            Err(code) => return code,
        };
        match self
            .keyed(
                reqwest::Method::DELETE,
                &format!("/api/v1/projects/{id}/authorship/user-token"),
                None,
            )
            .await
        {
            Ok(a) => self.finish(&a, |_| "user token removed".to_string()),
            Err(code) => code,
        }
    }

    pub async fn project_list(&self) -> ExitCode {
        match self.get("/api/v1/projects?limit=200").await {
            Ok(a) => self.finish(&a, |b| {
                lines(b, |p| {
                    format!(
                        "{}  {}  {}/{}",
                        text(&p["id"]),
                        text(&p["name"]),
                        text(&p["workspace"]["full_name"]),
                        text(&p["workspace"]["project_path"])
                    )
                })
            }),
            Err(code) => code,
        }
    }

    async fn keyed(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Answer, ExitCode> {
        let mut req = self
            .http
            .request(method, format!("{}{path}", self.base))
            .header("Idempotency-Key", Uuid::now_v7().to_string());
        if let Some(body) = body {
            req = req.json(body);
        }
        self.send(req).await
    }

    pub async fn secret_set(&self, project: &str, name: &str, value: &SecretText) -> ExitCode {
        let id = match self.project_id(project).await {
            Ok(id) => id,
            Err(code) => return code,
        };
        let body = json!({ "value": value.expose() });
        match self
            .keyed(
                reqwest::Method::PUT,
                &format!("/api/v1/projects/{id}/secrets/{name}"),
                Some(&body),
            )
            .await
        {
            Ok(a) => self.finish(&a, |_| format!("secret {name} set")),
            Err(code) => code,
        }
    }

    pub async fn auth_pairing(&self, purpose: &str) -> ExitCode {
        match self
            .post(
                "/api/v1/auth/pairings",
                &json!({ "purpose": purpose }),
                None,
            )
            .await
        {
            Ok(a) => self.finish(&a, |b| {
                format!(
                    "{} code {}  expires {}",
                    text(&b["purpose"]),
                    text(&b["code"]),
                    text(&b["expires_at"])
                )
            }),
            Err(code) => code,
        }
    }

    pub async fn auth_redeem(
        &self,
        code: &str,
        name: &str,
        out: Option<&std::path::Path>,
    ) -> ExitCode {
        let body = json!({ "code": code, "name": name });
        let req = self
            .http
            .post(format!("{}/api/v1/auth/pair", self.base))
            .json(&body);
        let a = match self.send(req).await {
            Ok(a) => a,
            Err(code) => return code,
        };
        if !a.ok() {
            if self.json {
                out_text(&a.body.to_string());
            }
            return fail(&a);
        }
        let token = text(&a.body["token"]);
        match out {
            Some(path) => {
                if let Err(e) = write_token(path, &token) {
                    err(&format!(
                        "error: token_file_unwritable {}",
                        json!({ "path": path.display().to_string(), "reason": e.to_string() })
                    ));
                    return ExitCode::FAILURE;
                }
                out_text(&format!(
                    "device {} paired; token written to {}",
                    text(&a.body["device_id"]),
                    path.display()
                ));
                ExitCode::SUCCESS
            }
            None => self.done(&a, |b| {
                format!(
                    "device {}\ntoken {}",
                    text(&b["device_id"]),
                    text(&b["token"])
                )
            }),
        }
    }

    pub async fn auth_devices(&self) -> ExitCode {
        match self.get("/api/v1/auth/devices").await {
            Ok(a) => self.finish(&a, |b| {
                let devices = b["devices"].as_array().into_iter().flatten().map(|d| {
                    format!(
                        "device\t{}\t{}\t{}\t{}",
                        text(&d["id"]),
                        text(&d["kind"]),
                        text(&d["name"]),
                        d["last_used_at"].as_str().unwrap_or("never")
                    )
                });
                let credentials = b["credentials"].as_array().into_iter().flatten().map(|c| {
                    format!(
                        "passkey\t{}\t{}\t{}",
                        text(&c["id"]),
                        text(&c["name"]),
                        c["last_used_at"].as_str().unwrap_or("never")
                    )
                });
                devices.chain(credentials).collect::<Vec<_>>().join("\n")
            }),
            Err(code) => code,
        }
    }

    pub async fn auth_revoke(&self, id: &str, passkey: bool) -> ExitCode {
        let (path, what) = if passkey {
            (format!("/api/v1/auth/credentials/{id}/revoke"), "passkey")
        } else {
            (format!("/api/v1/auth/devices/{id}/revoke"), "device")
        };
        match self.post(&path, &json!({}), None).await {
            Ok(a) => self.finish(&a, |b| format!("{what} {} revoked", text(&b["id"]))),
            Err(code) => code,
        }
    }

    pub async fn secret_list(&self, project: &str) -> ExitCode {
        let id = match self.project_id(project).await {
            Ok(id) => id,
            Err(code) => return code,
        };
        match self.get(&format!("/api/v1/projects/{id}/secrets")).await {
            Ok(a) => self.finish(&a, |body| {
                body["items"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|s| format!("{}\t{}", text(&s["name"]), text(&s["updated_at"])))
                    .collect::<Vec<_>>()
                    .join("\n")
            }),
            Err(code) => code,
        }
    }

    pub async fn inbox_list(&self, project: Option<&str>, limit: u32) -> ExitCode {
        let mut path = format!("/api/v1/inbox?limit={limit}");
        if let Some(project) = project {
            match self.project_id(project).await {
                Ok(id) => path.push_str(&format!("&project_id={id}")),
                Err(code) => return code,
            }
        }
        match self.get(&path).await {
            Ok(a) => self.finish(&a, |body| {
                lines(body, |i| {
                    let what = i["title"]["task_key"]
                        .as_str()
                        .or_else(|| i["title"]["spec_slug"].as_str())
                        .unwrap_or("-");
                    format!(
                        "{}  {}/{}  {}  {}  {}",
                        text(&i["id"]),
                        text(&i["kind"]),
                        text(&i["subject"]),
                        text(&i["project_name"]),
                        what,
                        text(&i["action"]["kind"])
                    )
                })
            }),
            Err(code) => code,
        }
    }

    pub async fn inbox_dismiss(&self, id: &str, reason: &str) -> ExitCode {
        match self
            .post(
                &format!("/api/v1/inbox/{id}/dismiss"),
                &json!({ "reason": reason }),
                None,
            )
            .await
        {
            Ok(a) => self.finish(&a, |b| format!("{} {}", text(&b["id"]), text(&b["state"]))),
            Err(code) => code,
        }
    }

    pub async fn hook_list(&self, project: &str) -> ExitCode {
        let id = match self.project_id(project).await {
            Ok(id) => id,
            Err(code) => return code,
        };
        match self.get(&format!("/api/v1/projects/{id}/hooks")).await {
            Ok(a) => self.finish(&a, |body| {
                lines(body, |h| {
                    format!(
                        "{}  on {}  {}  {}",
                        text(&h["name"]),
                        text(&h["on"]),
                        text(&h["mode"]),
                        text(&h["action"])
                    )
                })
            }),
            Err(code) => code,
        }
    }

    pub async fn hook_runs(
        &self,
        project: &str,
        hook: Option<&str>,
        state: Option<&str>,
        limit: u32,
    ) -> ExitCode {
        let id = match self.project_id(project).await {
            Ok(id) => id,
            Err(code) => return code,
        };
        let mut path = format!("/api/v1/hook-runs?project_id={id}&limit={limit}");
        for (key, value) in [("hook", hook), ("state", state)] {
            if let Some(value) = value {
                path.push_str(&format!("&{key}={value}"));
            }
        }
        match self.get(&path).await {
            Ok(a) => self.finish(&a, |body| {
                lines(body, |r| {
                    format!(
                        "{}  {}  {}  {}  {}",
                        text(&r["id"]),
                        text(&r["hook"]),
                        text(&r["trigger"]),
                        text(&r["state"]),
                        text(&r["queued_at"])
                    )
                })
            }),
            Err(code) => code,
        }
    }

    pub async fn deploy_list(
        &self,
        project: &str,
        environment: Option<&str>,
        state: Option<&str>,
        limit: u32,
    ) -> ExitCode {
        let id = match self.project_id(project).await {
            Ok(id) => id,
            Err(code) => return code,
        };
        let mut path = format!("/api/v1/projects/{id}/deployments?limit={limit}");
        for (key, value) in [("environment", environment), ("state", state)] {
            if let Some(value) = value {
                path.push_str(&format!("&{key}={value}"));
            }
        }
        match self.get(&path).await {
            Ok(a) => self.finish(&a, |body| lines(body, deployment_line)),
            Err(code) => code,
        }
    }

    pub async fn deploy_show(&self, id: &str) -> ExitCode {
        match self.get(&format!("/api/v1/deployments/{id}")).await {
            Ok(a) => self.finish(&a, deployment_detail),
            Err(code) => code,
        }
    }

    pub async fn deploy_redeploy(
        &self,
        project: &str,
        environment: &str,
        reason: &str,
    ) -> ExitCode {
        let id = match self.project_id(project).await {
            Ok(id) => id,
            Err(code) => return code,
        };
        match self
            .post(
                &format!("/api/v1/projects/{id}/environments/{environment}/actions/deploy"),
                &json!({ "reason": reason }),
                None,
            )
            .await
        {
            Ok(a) => self.finish(&a, deployment_line),
            Err(code) => code,
        }
    }

    pub async fn deploy_cancel(&self, id: &str, reason: &str) -> ExitCode {
        match self
            .post(
                &format!("/api/v1/deployments/{id}/actions/cancel"),
                &json!({ "reason": reason }),
                None,
            )
            .await
        {
            Ok(a) => self.finish(&a, deployment_line),
            Err(code) => code,
        }
    }

    pub async fn hook_rerun(&self, id: &str, reason: &str) -> ExitCode {
        match self
            .post(
                &format!("/api/v1/hook-runs/{id}/actions/rerun"),
                &json!({ "reason": reason }),
                None,
            )
            .await
        {
            Ok(a) => self.finish(&a, |b| format!("{} {}", text(&b["id"]), text(&b["state"]))),
            Err(code) => code,
        }
    }

    pub async fn secret_rm(&self, project: &str, name: &str) -> ExitCode {
        let id = match self.project_id(project).await {
            Ok(id) => id,
            Err(code) => return code,
        };
        match self
            .keyed(
                reqwest::Method::DELETE,
                &format!("/api/v1/projects/{id}/secrets/{name}"),
                None,
            )
            .await
        {
            Ok(a) => self.finish(&a, |_| format!("secret {name} removed")),
            Err(code) => code,
        }
    }

    pub async fn scm_link(&self, integration: &str, wait: bool) -> ExitCode {
        let base = format!("/api/v1/scm/{integration}/user-link/authorizations");
        let mut current = match self.post(&base, &json!({}), None).await {
            Ok(a) if a.ok() => a.body,
            Ok(a) => {
                if self.json {
                    out(&a.body.to_string());
                }
                return fail(&a);
            }
            Err(code) => return code,
        };
        let prompt = format!(
            "Open {} and enter {}",
            text(&current["verification_uri"]),
            text(&current["user_code"])
        );
        if !wait {
            if self.json {
                out(&current.to_string());
            } else {
                out(&prompt);
            }
            return ExitCode::SUCCESS;
        }
        if self.json {
            err(&prompt);
        } else {
            out(&prompt);
        }
        let id = text(&current["id"]);
        while current["state"] == "pending" {
            let secs = current["interval_secs"].as_u64().unwrap_or(5).max(1);
            tokio::time::sleep(std::time::Duration::from_secs(secs)).await;
            current = match self.get(&format!("{base}/{id}")).await {
                Ok(a) if a.ok() => a.body,
                Ok(a) => return fail(&a),
                Err(code) => return code,
            };
        }
        if self.json {
            out(&current.to_string());
        }
        match text(&current["state"]).as_str() {
            "completed" => {
                if !self.json {
                    out(&format!("linked {}", text(&current["link"]["login"])));
                }
                ExitCode::SUCCESS
            }
            "failed" => {
                refuse(
                    &text(&current["failure"]["code"]),
                    &current["failure"]["params"],
                );
                ExitCode::FAILURE
            }
            state => {
                refuse(state, &json!({}));
                ExitCode::FAILURE
            }
        }
    }

    pub async fn scm_status(&self, integration: &str) -> ExitCode {
        match self
            .get(&format!("/api/v1/scm/{integration}/user-link"))
            .await
        {
            Ok(a) => self.finish(&a, |b| {
                let cause = b["relink_cause"]
                    .as_str()
                    .map_or(String::new(), |c| format!(" ({c})"));
                format!(
                    "{}{cause} {} refresh_expires_at {}",
                    text(&b["state"]),
                    text(&b["login"]),
                    text(&b["refresh_expires_at"])
                )
            }),
            Err(code) => code,
        }
    }

    pub async fn scm_unlink(&self, integration: &str) -> ExitCode {
        let path = format!("/api/v1/scm/{integration}/user-link");
        match self.keyed(reqwest::Method::DELETE, &path, None).await {
            Ok(a) if a.ok() => self.done(&a, |_| format!("unlinked {integration}")),
            Ok(a) => {
                if self.json {
                    out(&a.body.to_string());
                }
                fail(&a);
                ExitCode::FAILURE
            }
            Err(code) => code,
        }
    }

    pub(crate) async fn project_id(&self, project: &str) -> Result<String, ExitCode> {
        if project.parse::<Uuid>().is_ok() {
            return Ok(project.to_string());
        }
        let a = self.get("/api/v1/projects?limit=200").await?;
        if !a.ok() {
            return Err(fail(&a));
        }
        a.body["items"]
            .as_array()
            .and_then(|items| items.iter().find(|p| p["name"] == project))
            .and_then(|p| p["id"].as_str())
            .map(String::from)
            .ok_or_else(|| {
                err(&format!(
                    "error: not_found {}",
                    json!({ "project": project })
                ));
                ExitCode::FAILURE
            })
    }

    pub async fn spec_approve(&self, project: &str, path: &str, reason: Option<&str>) -> ExitCode {
        let id = match self.project_id(project).await {
            Ok(id) => id,
            Err(code) => return code,
        };
        let registered = match self
            .post(
                &format!("/api/v1/projects/{id}/specs"),
                &json!({ "path": path }),
                None,
            )
            .await
        {
            Ok(a) => a,
            Err(code) => return code,
        };
        if !registered.ok() {
            return self.finish(&registered, |_| String::new());
        }
        if registered.body["lint"]["ok"] != true {
            return self.lint_failed(&registered.body["lint"]["issues"], &registered.body);
        }
        let tag = registered
            .etag
            .clone()
            .unwrap_or_else(|| format!("\"{}\"", text(&registered.body["revision"]["id"])));
        let spec = text(&registered.body["id"]);
        let approved = match self
            .post(
                &format!("/api/v1/specs/{spec}/approve"),
                &json!({ "reason": reason }),
                Some(&tag),
            )
            .await
        {
            Ok(a) => a,
            Err(code) => return code,
        };
        if approved.body["code"] == "spec_not_ready" {
            return self.lint_failed(&approved.body["params"]["issues"], &approved.body);
        }
        if approved.body["code"] == "revision_conflict" {
            if self.json {
                out(&approved.body.to_string());
            } else {
                for step in approved.body["params"]["steps"]
                    .as_array()
                    .into_iter()
                    .flatten()
                {
                    out(&conflict_line(step));
                }
            }
            return ExitCode::from(EXIT_CONFLICT);
        }
        let plan = registered.body["revision_plan"].clone();
        self.finish(&approved, |b| {
            let mut lines: Vec<String> = b["lint"]["issues"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|i| i["severity"] == "warning")
                .map(issue_line)
                .collect();
            let steps = b["steps"].as_array().map_or(0, Vec::len);
            if plan.is_null() {
                lines.push(format!("approved {} ({steps} steps)", text(&b["slug"])));
            } else {
                let count = |actions: &[&str]| {
                    plan["steps"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter(|s| actions.iter().any(|a| s["action"] == *a))
                        .count()
                };
                lines.push(format!(
                    "approved {} ({steps} steps: {} kept, {} repinned, {} added, {} superseded)",
                    text(&b["slug"]),
                    count(&["keep"]),
                    count(&["repin", "restore"]),
                    count(&["add"]),
                    count(&["supersede"]),
                ));
            }
            lines.join("\n")
        })
    }

    async fn spec_id(&self, project: &str, path: &str) -> Result<String, ExitCode> {
        let id = self.project_id(project).await?;
        let mut cursor = String::new();
        loop {
            let a = self
                .get(&format!(
                    "/api/v1/projects/{id}/specs?limit=200&cursor={cursor}"
                ))
                .await?;
            if !a.ok() {
                return Err(fail(&a));
            }
            if let Some(spec) = a.body["items"]
                .as_array()
                .and_then(|items| items.iter().find(|s| s["path"] == path))
            {
                return Ok(text(&spec["id"]));
            }
            match a.body["next_cursor"].as_str() {
                Some(next) => cursor = next.to_string(),
                None => {
                    err(&format!(
                        "error: not_found {}",
                        json!({ "project": project, "path": path })
                    ));
                    return Err(ExitCode::FAILURE);
                }
            }
        }
    }

    pub async fn spec_withdraw(&self, project: &str, path: &str, reason: &str) -> ExitCode {
        let spec = match self.spec_id(project, path).await {
            Ok(id) => id,
            Err(code) => return code,
        };
        match self
            .post(
                &format!("/api/v1/specs/{spec}/withdraw"),
                &json!({ "reason": reason }),
                None,
            )
            .await
        {
            Ok(a) => self.finish(&a, |b| format!("withdrawn {}", text(&b["slug"]))),
            Err(code) => code,
        }
    }

    fn lint_failed(&self, issues: &Value, body: &Value) -> ExitCode {
        if self.json {
            out(&body.to_string());
        } else {
            for issue in issues.as_array().into_iter().flatten() {
                out(&issue_line(issue));
            }
        }
        ExitCode::from(EXIT_PROBLEMS)
    }

    pub async fn spec_quick(&self, project: &str, body: &Value) -> ExitCode {
        let id = match self.project_id(project).await {
            Ok(id) => id,
            Err(code) => return code,
        };
        let answer = match self
            .post(&format!("/api/v1/projects/{id}/quick-specs"), body, None)
            .await
        {
            Ok(a) => a,
            Err(code) => return code,
        };
        if answer.body["code"] == "spec_not_ready" {
            err("error: spec_not_ready");
            for issue in answer.body["params"]["issues"]
                .as_array()
                .into_iter()
                .flatten()
            {
                err(&issue_line(issue));
            }
            return ExitCode::FAILURE;
        }
        self.finish(&answer, |b| {
            let spec = &b["spec"];
            let mut lines = vec![
                format!("spec {} ({})", text(&spec["slug"]), text(&spec["state"])),
                format!("path {}", text(&b["path"])),
                format!(
                    "commit {}",
                    text(&b["commit_sha"]).chars().take(12).collect::<String>()
                ),
            ];
            lines.extend(
                b["work_items"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|w| format!("task {}", text(&w["task_key"]))),
            );
            lines.join("\n")
        })
    }

    pub async fn spec_template(&self, locale: &str, kind: &str) -> ExitCode {
        let a = match self
            .get(&format!(
                "/api/v1/spec-template?locale={locale}&kind={kind}"
            ))
            .await
        {
            Ok(a) => a,
            Err(code) => return code,
        };
        if !a.ok() || self.json {
            return self.finish(&a, |_| String::new());
        }
        write!(std::io::stdout(), "{}", text(&a.body["content"])).best_effort("print the content");
        ExitCode::SUCCESS
    }

    pub async fn task_list(
        &self,
        project: Option<&str>,
        spec: Option<&str>,
        state: Option<&str>,
    ) -> ExitCode {
        let mut query = vec!["limit=200".to_string()];
        for (k, v) in [("project_id", project), ("spec_id", spec), ("state", state)] {
            if let Some(v) = v {
                query.push(format!("{k}={v}"));
            }
        }
        match self
            .get(&format!("/api/v1/work-items?{}", query.join("&")))
            .await
        {
            Ok(a) => self.finish(&a, |b| {
                lines(b, |t| {
                    let place = match (t["stage"].as_str(), t["stage_phase"].as_str()) {
                        (Some(s), Some(p)) => format!("{s}/{p}"),
                        (Some(s), None) => s.to_string(),
                        _ => String::new(),
                    };
                    let blocker = t["blocker"]["code"].as_str().unwrap_or_default();
                    let queue = match (
                        t["merge_queue"]["base_branch"].as_str(),
                        t["merge_queue"]["position"].as_u64(),
                        t["merge_queue"]["size"].as_u64(),
                    ) {
                        (Some(branch), Some(0), _) => format!("queue: {branch}, its turn"),
                        (Some(branch), Some(n), Some(size)) => {
                            format!("queue: {branch} {n} of {size}")
                        }
                        _ => String::new(),
                    };
                    format!(
                        "{}  {}  {} {} {} {}",
                        text(&t["id"]),
                        text(&t["task_key"]),
                        text(&t["state"]),
                        place,
                        blocker,
                        queue
                    )
                    .trim_end()
                    .to_string()
                })
            }),
            Err(code) => code,
        }
    }

    pub async fn task_merge(&self, id: &str, reason: Option<&str>) -> ExitCode {
        self.task_versioned(id, "merge", reason, "merging").await
    }

    pub async fn task_revert(&self, id: &str, reason: &str) -> ExitCode {
        self.task_versioned(id, "revert", Some(reason), "revert")
            .await
    }

    pub async fn task_show(&self, id: &str) -> ExitCode {
        match self.get(&format!("/api/v1/work-items/{id}")).await {
            Ok(a) => self.finish(&a, |b| {
                let place = match (b["stage"].as_str(), b["stage_phase"].as_str()) {
                    (Some(s), Some(p)) => format!("{s}/{p}"),
                    (Some(s), None) => s.to_string(),
                    _ => String::new(),
                };
                let mut out = format!(
                    "{} {} {} {}",
                    text(&b["task_key"]),
                    text(&b["state"]),
                    place,
                    b["blocker"]["code"].as_str().unwrap_or_default()
                )
                .trim_end()
                .to_string();
                if let Some(key) = b["revert_of"]["key"].as_str() {
                    out.push_str(&format!("\nreverts {key}"));
                }
                if let Some(key) = b["reverted_by"]["key"].as_str() {
                    out.push_str(&format!(
                        "\nreverted by {key} {}",
                        text(&b["reverted_by"]["state"])
                    ));
                }
                out
            }),
            Err(code) => code,
        }
    }

    pub async fn task_start(&self, id: &str) -> ExitCode {
        self.task_versioned(id, "start", None, "starting").await
    }

    async fn task_versioned(
        &self,
        id: &str,
        action: &str,
        reason: Option<&str>,
        verb: &str,
    ) -> ExitCode {
        let current = match self.get(&format!("/api/v1/work-items/{id}")).await {
            Ok(a) if a.ok() => a,
            Ok(a) => return fail(&a),
            Err(code) => return code,
        };
        let tag = current
            .etag
            .clone()
            .unwrap_or_else(|| format!("W/\"{}\"", current.body["version"]));
        match self
            .post(
                &format!("/api/v1/work-items/{id}/actions/{action}"),
                &json!({ "reason": reason }),
                Some(&tag),
            )
            .await
        {
            Ok(a) => self.finish(&a, |b| format!("{verb} {}", text(&b["task_key"]))),
            Err(code) => code,
        }
    }

    pub async fn review(&self, number: u64, project: Option<&str>, repo: Option<&str>) -> ExitCode {
        let id = match project {
            Some(project) => self.project_id(project).await,
            None => self.only_project_id().await,
        };
        let id = match id {
            Ok(id) => id,
            Err(code) => return code,
        };
        let body = json!({ "repo": repo, "number": number });
        match self
            .post(
                &format!("/api/v1/projects/{id}/external-changes"),
                &body,
                None,
            )
            .await
        {
            Ok(a) => self.finish(&a, |b| {
                format!(
                    "review requested {} {}",
                    text(&b["task_key"]),
                    text(&b["stage_phase"])
                )
            }),
            Err(code) => code,
        }
    }

    async fn only_project_id(&self) -> Result<String, ExitCode> {
        let a = self.get("/api/v1/projects?limit=2").await?;
        if !a.ok() {
            return Err(fail(&a));
        }
        match a.body["items"].as_array().map(Vec::as_slice) {
            Some([only]) => Ok(text(&only["id"])),
            _ => {
                err("error: project_required pass --project <id or name>");
                Err(ExitCode::from(2))
            }
        }
    }

    pub async fn task_action(&self, id: &str, action: &str, reason: &str) -> ExitCode {
        match self
            .post(
                &format!("/api/v1/work-items/{id}/actions/{action}"),
                &json!({ "reason": reason }),
                None,
            )
            .await
        {
            Ok(a) => self.finish(&a, |b| {
                format!("{} {}", text(&b["task_key"]), text(&b["state"]))
            }),
            Err(code) => code,
        }
    }

    pub async fn task_fix(&self, id: &str) -> ExitCode {
        match self
            .post(
                &format!("/api/v1/work-items/{id}/actions/fix"),
                &json!({}),
                None,
            )
            .await
        {
            Ok(a) => self.finish(&a, |b| {
                format!("fix started {} {}", text(&b["task_key"]), text(&b["state"]))
            }),
            Err(code) => code,
        }
    }

    pub async fn task_return(&self, id: &str, stage: Option<&str>, reason: &str) -> ExitCode {
        match self
            .post(
                &format!("/api/v1/work-items/{id}/actions/return"),
                &json!({ "stage": stage, "reason": reason }),
                None,
            )
            .await
        {
            Ok(a) => self.finish(&a, |b| {
                format!("{} {}", text(&b["task_key"]), text(&b["state"]))
            }),
            Err(code) => code,
        }
    }

    pub async fn permission_list(&self, state: &str) -> ExitCode {
        match self
            .get(&format!(
                "/api/v1/permission-requests?state={state}&limit=200"
            ))
            .await
        {
            Ok(a) => self.finish(&a, |b| {
                lines(b, |p| {
                    format!(
                        "{}  {}  {}  {}",
                        text(&p["id"]),
                        text(&p["state"]),
                        text(&p["tool"]),
                        text(&p["summary"])
                    )
                })
            }),
            Err(code) => code,
        }
    }

    async fn session_at(&self, resource: &str) -> Result<String, ExitCode> {
        match self.get(resource).await {
            Ok(a) if a.ok() => match a.body["session"]["id"].as_str() {
                Some(id) => Ok(id.to_string()),
                None => {
                    err("error: invalid_state {\"state\":\"no_session\"}");
                    Err(ExitCode::from(EXIT_CONFLICT))
                }
            },
            Ok(a) => Err(fail(&a)),
            Err(code) => Err(code),
        }
    }

    async fn post_to_session(&self, session: &str, action: &str, body: &Value) -> ExitCode {
        match self
            .post(&format!("/api/v1/sessions/{session}/{action}"), body, None)
            .await
        {
            Ok(a) => self.finish(&a, |b| format!("{} {}", text(&b["id"]), text(&b["state"]))),
            Err(code) => code,
        }
    }

    pub async fn task_session(&self, task: &str, action: &str, body: &Value) -> ExitCode {
        match self.session_at(&format!("/api/v1/work-items/{task}")).await {
            Ok(session) => self.post_to_session(&session, action, body).await,
            Err(code) => code,
        }
    }

    pub async fn task_timeline(&self, task: &str) -> ExitCode {
        let sessions = match self
            .get(&format!("/api/v1/sessions?work_item_id={task}"))
            .await
        {
            Ok(a) if a.ok() => a.body,
            Ok(a) => return fail(&a),
            Err(code) => return code,
        };
        let ids: Vec<String> = sessions["items"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|s| text(&s["id"]))
            .collect();
        self.print_timeline(&ids).await
    }

    async fn print_timeline(&self, sessions: &[String]) -> ExitCode {
        let mut rows = Vec::new();
        for id in sessions {
            let mut after: Option<u64> = None;
            loop {
                let cursor = after.map(|a| format!("&after={a}")).unwrap_or_default();
                let page = match self
                    .get(&format!("/api/v1/sessions/{id}/timeline?limit=500{cursor}"))
                    .await
                {
                    Ok(a) if a.ok() => a.body,
                    Ok(a) => return fail(&a),
                    Err(code) => return code,
                };
                for item in page["items"].as_array().into_iter().flatten() {
                    let mut row = item.clone();
                    row["session_id"] = Value::String(id.clone());
                    rows.push(row);
                }
                match page["next_after"].as_u64() {
                    Some(next) => after = Some(next),
                    None => break,
                }
            }
        }
        if self.json {
            out(&json!({ "items": rows }).to_string());
        } else {
            for r in &rows {
                out(&format!(
                    "{}  {}  {}",
                    text(&r["at"]),
                    text(&r["kind"]),
                    text(&r["summary"]).replace('\n', " ")
                ));
            }
        }
        ExitCode::SUCCESS
    }

    pub async fn spec_draft(&self, new: &NewDraft<'_>) -> ExitCode {
        let id = match self.project_id(new.project).await {
            Ok(id) => id,
            Err(code) => return code,
        };
        let body = json!({
            "idea": new.idea,
            "path": new.path,
            "context_paths": new.context,
            "locale": new.locale,
            "mode": if new.session { "session" } else { "draft" },
            "target_kind": if new.revise { "revise" } else { "new" },
        });
        let created = match self
            .post(&format!("/api/v1/projects/{id}/spec-drafts"), &body, None)
            .await
        {
            Ok(a) => a,
            Err(code) => return code,
        };
        if !created.ok() || !new.wait {
            return self.finish(&created, |b| text(&b["id"]));
        }
        let draft = text(&created.body["id"]);
        if !self.json {
            out(&draft);
        }
        loop {
            let current = match self.get(&format!("/api/v1/spec-drafts/{draft}")).await {
                Ok(a) if a.ok() => a.body,
                Ok(a) => return fail(&a),
                Err(code) => return code,
            };
            let state = text(&current["state"]);
            if state == "awaiting_operator" {
                if self.json {
                    out(&current.to_string());
                } else {
                    out(&draft_summary(&current));
                }
                return ExitCode::SUCCESS;
            }
            if matches!(state.as_str(), "succeeded" | "failed" | "cancelled") {
                return self.drafted(&current);
            }
            tokio::time::sleep(DRAFT_POLL).await;
        }
    }

    fn drafted(&self, draft: &Value) -> ExitCode {
        if self.json {
            out(&draft.to_string());
        }
        let failure = &draft["failure"];
        match text(&draft["state"]).as_str() {
            "succeeded" => {
                if !self.json {
                    out(&format!(
                        "succeeded spec {} commit {}",
                        text(&draft["spec_id"]),
                        text(&draft["commit"])
                    ));
                }
                ExitCode::SUCCESS
            }
            "failed" if failure["code"] == "lint_rounds_exhausted" => {
                if !self.json {
                    for issue in draft["last_round"]["issues"]
                        .as_array()
                        .into_iter()
                        .flatten()
                    {
                        out(&issue_line(issue));
                    }
                }
                err(&format!(
                    "error: lint_rounds_exhausted {}",
                    failure["params"]
                ));
                ExitCode::from(EXIT_PROBLEMS)
            }
            "failed" => {
                err(&format!(
                    "error: {} {}",
                    text(&failure["code"]),
                    failure["params"]
                ));
                ExitCode::FAILURE
            }
            _ => {
                err("error: cancelled {}");
                ExitCode::from(EXIT_CONFLICT)
            }
        }
    }

    pub async fn draft_list(&self, project: &str, state: &str) -> ExitCode {
        let id = match self.project_id(project).await {
            Ok(id) => id,
            Err(code) => return code,
        };
        let mut items: Vec<Value> = Vec::new();
        let mut cursor = String::new();
        loop {
            let a = match self
                .get(&format!(
                    "/api/v1/projects/{id}/spec-drafts?state={state}&limit=200&cursor={cursor}"
                ))
                .await
            {
                Ok(a) if a.ok() => a,
                Ok(a) => return self.finish(&a, |_| String::new()),
                Err(code) => return code,
            };
            items.extend(a.body["items"].as_array().cloned().unwrap_or_default());
            match a.body["next_cursor"].as_str() {
                Some(next) => cursor = next.to_string(),
                None => break,
            }
        }
        if self.json {
            out(&json!({ "items": items }).to_string());
        } else {
            for d in &items {
                out(&draft_line(d));
            }
        }
        ExitCode::SUCCESS
    }

    pub async fn issue_list(&self, project: &str, state: Option<&str>) -> ExitCode {
        let id = match self.project_id(project).await {
            Ok(id) => id,
            Err(code) => return code,
        };
        let filter = state.map(|s| format!("&state={s}")).unwrap_or_default();
        let mut items: Vec<Value> = Vec::new();
        let mut cursor = String::new();
        loop {
            let a = match self
                .get(&format!(
                    "/api/v1/projects/{id}/issue-intakes?limit=200&cursor={cursor}{filter}"
                ))
                .await
            {
                Ok(a) if a.ok() => a,
                Ok(a) => return self.finish(&a, |_| String::new()),
                Err(code) => return code,
            };
            items.extend(a.body["items"].as_array().cloned().unwrap_or_default());
            match a.body["next_cursor"].as_str() {
                Some(next) => cursor = next.to_string(),
                None => break,
            }
        }
        if self.json {
            out(&json!({ "items": items }).to_string());
        } else {
            for i in &items {
                out(&intake_line(i));
            }
        }
        ExitCode::SUCCESS
    }

    pub async fn issue_intake(&self, project: &str, number: u64, repo: Option<&str>) -> ExitCode {
        let id = match self.project_id(project).await {
            Ok(id) => id,
            Err(code) => return code,
        };
        match self
            .post(
                &format!("/api/v1/projects/{id}/issue-intakes"),
                &json!({ "number": number, "repo": repo }),
                None,
            )
            .await
        {
            Ok(a) => self.finish(&a, intake_line),
            Err(code) => code,
        }
    }

    pub async fn issue_dismiss(&self, id: &str, reason: Option<String>) -> ExitCode {
        match self
            .post(
                &format!("/api/v1/issue-intakes/{id}/actions/dismiss"),
                &json!({ "reason": reason }),
                None,
            )
            .await
        {
            Ok(a) => self.finish(&a, intake_line),
            Err(code) => code,
        }
    }

    pub async fn issue_retry(&self, id: &str) -> ExitCode {
        match self
            .post(
                &format!("/api/v1/issue-intakes/{id}/actions/retry"),
                &json!({}),
                None,
            )
            .await
        {
            Ok(a) => self.finish(&a, intake_line),
            Err(code) => code,
        }
    }

    pub async fn draft_show(&self, id: &str) -> ExitCode {
        match self.get(&format!("/api/v1/spec-drafts/{id}")).await {
            Ok(a) => self.finish(&a, draft_summary),
            Err(code) => code,
        }
    }

    pub async fn draft_timeline(&self, id: &str) -> ExitCode {
        match self.session_at(&format!("/api/v1/spec-drafts/{id}")).await {
            Ok(session) => self.print_timeline(&[session]).await,
            Err(code) => code,
        }
    }

    pub async fn draft_session(&self, id: &str, action: &str, body: &Value) -> ExitCode {
        match self.session_at(&format!("/api/v1/spec-drafts/{id}")).await {
            Ok(session) => self.post_to_session(&session, action, body).await,
            Err(code) => code,
        }
    }

    pub async fn draft_operator(&self, id: &str, action: &str, body: &Value) -> ExitCode {
        match self
            .post(&format!("/api/v1/spec-drafts/{id}/{action}"), body, None)
            .await
        {
            Ok(a) => self.finish(&a, |b| format!("{} {}", text(&b["id"]), text(&b["state"]))),
            Err(code) => code,
        }
    }

    pub async fn draft_changes(&self, id: &str, diff: bool) -> ExitCode {
        match self.get(&format!("/api/v1/spec-drafts/{id}/changes")).await {
            Ok(a) => self.finish(&a, |b| changes_summary(b, diff)),
            Err(code) => code,
        }
    }

    pub async fn draft_action(&self, id: &str, action: &str, body: &Value) -> ExitCode {
        match self
            .post(
                &format!("/api/v1/spec-drafts/{id}/actions/{action}"),
                body,
                None,
            )
            .await
        {
            Ok(a) => self.finish(&a, |b| format!("{} {}", text(&b["id"]), text(&b["state"]))),
            Err(code) => code,
        }
    }

    pub async fn permission_rules(&self, project: &str) -> ExitCode {
        let id = match self.project_id(project).await {
            Ok(id) => id,
            Err(code) => return code,
        };
        match self
            .get(&format!("/api/v1/projects/{id}/permission-rules"))
            .await
        {
            Ok(a) => self.finish(&a, |b| {
                let project_rules = lines(b, rule_line);
                let operator = b["operator_rules"]
                    .as_array()
                    .map(|rules| {
                        rules
                            .iter()
                            .map(operator_rule_line)
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_default();
                [project_rules, operator]
                    .into_iter()
                    .filter(|part| !part.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n")
            }),
            Err(code) => code,
        }
    }

    pub async fn permission_rule_add(&self, rule: NewRule<'_>) -> ExitCode {
        let id = match self.project_id(rule.project).await {
            Ok(id) => id,
            Err(code) => return code,
        };
        let body = json!({
            "effect": rule.effect,
            "tool": rule.tool,
            "match": rule.match_kind,
            "command": rule.command,
            "path": rule.path,
            "note": rule.note,
        });
        match self
            .post(
                &format!("/api/v1/projects/{id}/permission-rules"),
                &body,
                None,
            )
            .await
        {
            Ok(a) => self.finish(&a, |b| format!("{} added", text(&b["id"]))),
            Err(code) => code,
        }
    }

    pub async fn permission_revoke(&self, id: &str, reason: &str) -> ExitCode {
        match self
            .post(
                &format!("/api/v1/permission-rules/{id}/revoke"),
                &json!({ "reason": reason }),
                None,
            )
            .await
        {
            Ok(a) => self.finish(&a, |b| format!("{} revoked", text(&b["id"]))),
            Err(code) => code,
        }
    }

    pub async fn decision_list(&self, state: &str) -> ExitCode {
        match self
            .get(&format!(
                "/api/v1/decision-requests?state={state}&limit=200"
            ))
            .await
        {
            Ok(a) => self.finish(&a, |b| {
                lines(b, |d| {
                    format!(
                        "{}  {}  {}",
                        text(&d["id"]),
                        text(&d["state"]),
                        text(&d["question"])
                    )
                })
            }),
            Err(code) => code,
        }
    }

    pub async fn decision_show(&self, id: &str) -> ExitCode {
        match self.get(&format!("/api/v1/decision-requests/{id}")).await {
            Ok(a) => self.finish(&a, decision_text),
            Err(code) => code,
        }
    }

    pub async fn decision_answer(
        &self,
        id: &str,
        option: Option<u32>,
        note: Option<&str>,
    ) -> ExitCode {
        match self
            .post(
                &format!("/api/v1/decision-requests/{id}/answer"),
                &json!({ "option": option, "note": note }),
                None,
            )
            .await
        {
            Ok(a) => self.finish(&a, |b| format!("{} {}", text(&b["id"]), text(&b["state"]))),
            Err(code) => code,
        }
    }

    pub async fn permission_answer(
        &self,
        id: &str,
        decision: &str,
        message: Option<&str>,
        remember: bool,
    ) -> ExitCode {
        match self
            .post(
                &format!("/api/v1/permission-requests/{id}/answer"),
                &json!({ "decision": decision, "message": message, "remember": remember }),
                None,
            )
            .await
        {
            Ok(a) => self.finish(&a, |b| format!("{} {}", text(&b["id"]), text(&b["state"]))),
            Err(code) => code,
        }
    }
}

fn text(v: &Value) -> String {
    v.as_str().map_or_else(|| v.to_string(), String::from)
}

fn decision_text(d: &Value) -> String {
    let mut out = vec![
        format!("{}  {}", text(&d["id"]), text(&d["state"])),
        text(&d["question"]),
    ];
    if let Some(context) = d["context"].as_str() {
        out.push(context.to_string());
    }
    for (i, option) in d["options"].as_array().into_iter().flatten().enumerate() {
        let mark = if i == 0 { " (recommended)" } else { "" };
        let detail = option["detail"]
            .as_str()
            .map_or_else(String::new, |t| format!(" - {t}"));
        out.push(format!(
            "  {}. {}{mark}{detail}",
            i + 1,
            text(&option["label"])
        ));
    }
    if !d["answer"].is_null() {
        let chosen = d["answer"]["option"]
            .as_u64()
            .map_or_else(|| "no option".to_string(), |o| format!("option {}", o + 1));
        let note = d["answer"]["note"]
            .as_str()
            .map_or_else(String::new, |n| format!(": {n}"));
        out.push(format!("answered: {chosen}{note}"));
    }
    out.join("\n")
}

fn rule_line(r: &Value) -> String {
    format!(
        "{}  {}  {}  {}  {}",
        text(&r["id"]),
        text(&r["effect"]),
        text(&r["match"]),
        text(&r["tool"]),
        r["command"]
            .as_str()
            .or_else(|| r["path"].as_str())
            .unwrap_or_default()
    )
    .trim_end()
    .to_string()
}

fn operator_rule_line(r: &Value) -> String {
    let list = |key: &str| {
        r[key]
            .as_array()
            .map(|items| items.iter().map(text).collect::<Vec<_>>().join(","))
            .unwrap_or_default()
    };
    format!(
        "operator:{}  {}  {}  {}",
        text(&r["id"]),
        text(&r["effect"]),
        list("tools"),
        [list("commands"), list("paths")].join(" ").trim()
    )
    .trim_end()
    .to_string()
}

fn lines(body: &Value, line: impl Fn(&Value) -> String) -> String {
    body["items"]
        .as_array()
        .map(|items| items.iter().map(&line).collect::<Vec<_>>().join("\n"))
        .unwrap_or_default()
}

fn deployment_line(d: &Value) -> String {
    let sha: String = d["sha"].as_str().unwrap_or("-").chars().take(8).collect();
    format!(
        "{}  {}  {}  {}  {}  {}",
        text(&d["id"]),
        text(&d["environment"]),
        text(&d["state"]),
        text(&d["cause"]),
        sha,
        text(&d["queued_at"])
    )
}

fn deployment_detail(d: &Value) -> String {
    let mut out = vec![deployment_line(d)];
    if let Some(error) = d["error"].as_object() {
        let phase = text(&d["phase"]);
        let code = error.get("code").map(text).unwrap_or_default();
        out.push(format!("failed at {phase}: {code}"));
    }
    for task in d["tasks"].as_array().into_iter().flatten() {
        out.push(format!(
            "task  {}  {}",
            text(&task["key"]),
            text(&task["gate"])
        ));
    }
    for check in d["checks"].as_array().into_iter().flatten() {
        out.push(format!(
            "check  {}  #{}  {}  {} ms",
            text(&check["phase"]),
            text(&check["ordinal"]),
            text(&check["state"]),
            text(&check["duration_ms"])
        ));
    }
    out.join("\n")
}

fn intake_line(i: &Value) -> String {
    format!(
        "{}  #{}  {}  {} {}  {}",
        text(&i["id"]),
        text(&i["issue"]["number"]),
        text(&i["state"]),
        text(&i["kind"]),
        text(&i["lane"]),
        text(&i["issue"]["title"])
    )
}

fn draft_line(d: &Value) -> String {
    format!(
        "{}  {}  {} {}",
        text(&d["id"]),
        text(&d["path"]),
        text(&d["state"]),
        d["failure"]["code"].as_str().unwrap_or_default()
    )
    .trim_end()
    .to_string()
}

fn changes_summary(c: &Value, diff: bool) -> String {
    let mut lines: Vec<String> = c["files"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|f| {
            let head = format!("{} {}", text(&f["status"]), text(&f["path"]));
            if diff {
                format!("{head}\n{}", text(&f["diff"]))
            } else {
                head
            }
        })
        .collect();
    let lint = &c["lint"];
    if lint["passed"] == true {
        lines.push("lint passed".to_string());
    } else {
        lines.push("lint failed".to_string());
    }
    for issue in lint["findings"].as_array().into_iter().flatten() {
        lines.push(issue_line(issue));
    }
    lines.join("\n")
}

fn draft_summary(d: &Value) -> String {
    let mut lines = vec![draft_line(d)];
    let attempt = &d["attempt"];
    if !attempt.is_null() {
        let round = &d["last_round"];
        let last = if round.is_null() {
            "no round yet".to_string()
        } else {
            format!(
                "round {} of {}: {}",
                round["round"],
                attempt["max_rounds"],
                text(&round["outcome"])
            )
        };
        lines.push(format!(
            "attempt {} ({}), {last}",
            attempt["number"],
            text(&attempt["state"])
        ));
    }
    for issue in d["last_round"]["issues"].as_array().into_iter().flatten() {
        lines.push(issue_line(issue));
    }
    if !d["failure"].is_null() {
        lines.push(format!(
            "failure {} {}",
            text(&d["failure"]["code"]),
            d["failure"]["params"]
        ));
    }
    if let Some(spec) = d["spec_id"].as_str() {
        lines.push(format!("spec {spec} commit {}", text(&d["commit"])));
    }
    let actions: Vec<String> = d["available_actions"]
        .as_array()
        .into_iter()
        .flatten()
        .map(text)
        .collect();
    if !actions.is_empty() {
        lines.push(format!("actions {}", actions.join(",")));
    }
    lines.join("\n")
}
