use std::io::Write;
use std::process::ExitCode;

use saka_core::secret::SecretText;
use serde_json::{Value, json};
use uuid::Uuid;
use zeroize::Zeroizing;

pub const EXIT_LINT: u8 = 3;
pub const EXIT_CONFLICT: u8 = 4;
const DRAFT_POLL: std::time::Duration = std::time::Duration::from_secs(2);

pub struct NewDraft<'a> {
    pub project: &'a str,
    pub path: &'a str,
    pub idea: &'a str,
    pub context: &'a [String],
    pub locale: Option<&'a str>,
    pub wait: bool,
}

pub struct Client {
    base: String,
    http: reqwest::Client,
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

fn out(text: &str) {
    let _ = writeln!(std::io::stdout(), "{text}");
}

fn err(text: &str) {
    let _ = writeln!(std::io::stderr(), "{text}");
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

// With a terminal on stdin the value is typed without echo; otherwise it is piped in.
pub fn read_secret(name: &str, multiline: bool) -> Result<SecretText, ExitCode> {
    use std::io::{BufRead, IsTerminal, Read};
    let stdin = std::io::stdin();
    let typed = stdin.is_terminal();
    let restore = if typed {
        let _ = write!(std::io::stderr(), "Value for {name}: ");
        let _ = std::io::stderr().flush();
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
        let _ = rustix::termios::tcsetattr(
            std::io::stdin(),
            rustix::termios::OptionalActions::Now,
            &saved,
        );
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
            json,
        }
    }

    async fn send(&self, req: reqwest::RequestBuilder) -> Result<Answer, ExitCode> {
        let response = req.send().await.map_err(|e| {
            err(&format!(
                "error: server_unreachable {}",
                json!({ "server": self.base, "reason": e.to_string() })
            ));
            ExitCode::FAILURE
        })?;
        let status = response.status().as_u16();
        let etag = response
            .headers()
            .get("etag")
            .and_then(|v| v.to_str().ok())
            .map(String::from);
        let body = response.json::<Value>().await.unwrap_or(Value::Null);
        Ok(Answer { status, body, etag })
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

    async fn project_id(&self, project: &str) -> Result<String, ExitCode> {
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
        ExitCode::from(EXIT_LINT)
    }

    pub async fn spec_template(&self, locale: &str) -> ExitCode {
        let a = match self
            .get(&format!("/api/v1/spec-template?locale={locale}"))
            .await
        {
            Ok(a) => a,
            Err(code) => return code,
        };
        if !a.ok() || self.json {
            return self.finish(&a, |_| String::new());
        }
        let _ = write!(std::io::stdout(), "{}", text(&a.body["content"]));
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
                    format!(
                        "{}  {}  {} {} {}",
                        text(&t["id"]),
                        text(&t["task_key"]),
                        text(&t["state"]),
                        place,
                        blocker
                    )
                    .trim_end()
                    .to_string()
                })
            }),
            Err(code) => code,
        }
    }

    pub async fn task_merge(&self, id: &str, reason: Option<&str>) -> ExitCode {
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
                &format!("/api/v1/work-items/{id}/actions/merge"),
                &json!({ "reason": reason }),
                Some(&tag),
            )
            .await
        {
            Ok(a) => self.finish(&a, |b| format!("merging {}", text(&b["task_key"]))),
            Err(code) => code,
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
                ExitCode::from(EXIT_LINT)
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
                lines(b, |r| {
                    format!(
                        "{}  {}  {}",
                        text(&r["id"]),
                        text(&r["tool"]),
                        r["command"].as_str().unwrap_or_default()
                    )
                    .trim_end()
                    .to_string()
                })
            }),
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

fn lines(body: &Value, line: impl Fn(&Value) -> String) -> String {
    body["items"]
        .as_array()
        .map(|items| items.iter().map(&line).collect::<Vec<_>>().join("\n"))
        .unwrap_or_default()
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
