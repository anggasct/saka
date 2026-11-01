use std::io::Write;
use std::process::ExitCode;

use serde_json::{Value, json};
use uuid::Uuid;

pub const EXIT_LINT: u8 = 3;
pub const EXIT_CONFLICT: u8 = 4;

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
        self.finish(&approved, |b| {
            let mut lines: Vec<String> = b["lint"]["issues"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|i| i["severity"] == "warning")
                .map(issue_line)
                .collect();
            let steps = b["steps"].as_array().map_or(0, Vec::len);
            lines.push(format!("approved {} ({steps} steps)", text(&b["slug"])));
            lines.join("\n")
        })
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

    pub async fn permission_answer(
        &self,
        id: &str,
        decision: &str,
        message: Option<&str>,
    ) -> ExitCode {
        match self
            .post(
                &format!("/api/v1/permission-requests/{id}/answer"),
                &json!({ "decision": decision, "message": message }),
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
