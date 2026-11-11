#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests fail loudly on a broken setup"
)]

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use saka_api::{ApiConfig, AppState, router};
use saka_core::diagnostics::{AgentLogin, HookDelivery, RepoGrant, RepoHooks, TokenStatus};
use saka_core::time::Timestamp;
use saka_core::vocab::Tool;
use saka_engine::DiagnosticLimits;
use saka_testkit::diagnostics::{
    FAKE_TOKEN_NAME, HostPort, HostScript, full_installation, saka_hook,
};
use saka_testkit::{Sim, SimConfig};
use serde_json::{Value, json};
use tower::ServiceExt;

struct Reply {
    status: StatusCode,
    cache_control: Option<String>,
    text: String,
    body: Value,
}

fn api(sim: &Sim) -> axum::Router {
    saka_testkit::logs::check_callsites_per_thread();
    let state = AppState::new(ApiConfig::new("0.1.0-test"));
    state.set_engine(sim.engine());
    router(state)
}

fn get(sim: &Sim, app: &axum::Router, path: &str) -> Reply {
    sim.block_on(async {
        let req = Request::get(path)
            .header("host", "127.0.0.1:7420")
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        let status = response.status();
        let cache_control = response
            .headers()
            .get("cache-control")
            .map(|v| v.to_str().unwrap().to_string());
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let text = String::from_utf8_lossy(&bytes).into_owned();
        Reply {
            status,
            cache_control,
            body: serde_json::from_str(&text).unwrap_or(Value::Null),
            text,
        }
    })
}

fn ids(body: &Value) -> Vec<(String, String)> {
    body["checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            (
                c["id"].as_str().unwrap().to_string(),
                c["subject"]["label"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

fn check<'a>(body: &'a Value, id: &str) -> &'a Value {
    body["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == id)
        .unwrap_or_else(|| panic!("no {id} in {body}"))
}

fn rows(sim: &Sim) -> (i64, i64, i64) {
    sim.engine()
        .store()
        .read(|c| {
            let n = |t: &str| {
                c.query_row(&format!("SELECT COUNT(*) FROM {t}"), [], |r| r.get(0))
                    .unwrap()
            };
            Ok((n("event"), n("job"), n("action_request")))
        })
        .unwrap()
}

#[test]
fn the_report_holds_every_check_in_catalog_order_and_changes_nothing() {
    let sim = Sim::new(SimConfig::default());
    let app = api(&sim);
    let before = rows(&sim);
    let r = get(&sim, &app, "/api/v1/diagnostics");
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.cache_control.as_deref(), Some("no-store"));
    let expected: Vec<(String, String)> = [
        ("server.ready", "instance"),
        ("db.migrations", "instance"),
        ("runtime.supervisor", "instance"),
        ("agent.binary", "claude:default"),
        ("agent.login", "claude:default"),
        ("agent.quota", "claude:default"),
        ("toolchain.cache", "instance"),
        ("toolchain.sources", "instance"),
        ("auth.config", "instance"),
        ("scm.credential", "ghes"),
        ("scm.credential", "github"),
        ("scm.repo_access", "acme/tasks"),
        ("scm.webhook", "ghes"),
        ("scm.webhook", "github"),
        ("scm.user_link", "ghes"),
        ("scm.user_link", "github"),
        ("project.user_token", "tasks"),
        ("project.config", "tasks"),
        ("toolchain.repo", "tasks/app"),
    ]
    .iter()
    .map(|(a, b)| (a.to_string(), b.to_string()))
    .collect();
    assert_eq!(ids(&r.body), expected);
    assert_eq!(r.body["mode"], "online");
    assert_eq!(r.body["server_version"], "0.1.0-test");
    let login = check(&r.body, "agent.login");
    assert_eq!(
        login["subject"],
        json!({ "kind": "agent_account", "id": "claude:default", "label": "claude:default" })
    );
    assert_eq!(login["status"], "pass");
    assert_eq!(
        login["params"],
        json!({ "account": "claude:default", "auth_method": "claude.ai" })
    );
    assert_eq!(login["fix"], Value::Null);
    assert_eq!(
        check(&r.body, "db.migrations")["code"],
        "migrations_current"
    );
    assert_eq!(check(&r.body, "agent.quota")["code"], "quota_unknown");
    let ready = check(&r.body, "server.ready");
    assert_eq!(
        ready["code"], "server_not_ready",
        "the sim runs no worker loop"
    );
    assert_eq!(ready["params"]["failing"], json!(["worker", "reconciler"]));
    let project = sim.project_id.0.to_string();
    let repo = sim.project().project.repos[0].repo_id.0.to_string();
    let subjects: Vec<(&str, Value)> = [
        "scm.credential",
        "scm.repo_access",
        "project.config",
        "toolchain.repo",
    ]
    .iter()
    .map(|id| {
        let c = r.body["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["id"] == *id && c["subject"]["label"] != "ghes")
            .unwrap();
        (*id, c["subject"].clone())
    })
    .collect();
    assert_eq!(
        subjects,
        [
            (
                "scm.credential",
                json!({ "kind": "integration", "id": "github", "label": "github" })
            ),
            (
                "scm.repo_access",
                json!({ "kind": "repo", "id": repo, "label": "acme/tasks" })
            ),
            (
                "project.config",
                json!({ "kind": "project", "id": project, "label": "tasks" })
            ),
            (
                "toolchain.repo",
                json!({ "kind": "project_repo", "id": format!("{project}/app"), "label": "tasks/app" })
            ),
        ]
    );
    assert_eq!(
        check(&r.body, "scm.credential")["code"],
        "unsupported_by_adapter",
        "ghes sorts first and the fake has no adapter for it"
    );
    assert_eq!(
        check(&r.body, "toolchain.repo")["params"],
        json!({ "toolchains": [] })
    );
    assert_eq!(
        r.body["summary"],
        json!({ "pass": 10, "warn": 0, "fail": 1, "skip": 8 })
    );
    assert_eq!(
        rows(&sim),
        before,
        "a diagnosis writes no event, job, or action"
    );
}

#[test]
fn the_check_filter_and_the_project_are_validated() {
    let sim = Sim::new(SimConfig::default());
    let app = api(&sim);
    let only = get(&sim, &app, "/api/v1/diagnostics?check=agent.login");
    assert_eq!(only.status, StatusCode::OK);
    assert_eq!(
        ids(&only.body),
        [("agent.login".to_string(), "claude:default".to_string())]
    );
    let two = get(
        &sim,
        &app,
        "/api/v1/diagnostics?check=toolchain.cache&check=agent.login",
    );
    assert_eq!(ids(&two.body).len(), 2);
    for id in ["scm.credential", "scm.repo_access", "scm.repo_webhook"] {
        let r = get(&sim, &app, &format!("/api/v1/diagnostics?check={id}"));
        assert_eq!(r.status, StatusCode::OK, "{id}: {}", r.text);
    }
    for (query, field, code) in [
        ("check=nope", "check", "unknown"),
        ("check=scm.app", "check", "unknown"),
        ("check=scm.installation", "check", "unknown"),
        ("check=cli.config", "check", "unknown"),
        ("project_id=not-a-uuid", "project_id", "invalid"),
        (
            "project_id=01900000-0000-7000-8000-000000000000",
            "project_id",
            "unknown",
        ),
    ] {
        let r = get(&sim, &app, &format!("/api/v1/diagnostics?{query}"));
        assert_eq!(
            r.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{query}: {}",
            r.text
        );
        assert_eq!(r.body["code"], "validation_failed");
        assert_eq!(
            r.body["params"]["errors"],
            json!([{ "field": field, "code": code }]),
            "{query}"
        );
    }
    let project = sim.project_id.0;
    let scoped = get(
        &sim,
        &app,
        &format!("/api/v1/diagnostics?project_id={project}"),
    );
    assert_eq!(scoped.status, StatusCode::OK, "{}", scoped.text);
}

fn limited() -> SimConfig {
    SimConfig {
        diagnostics: DiagnosticLimits {
            concurrency: 8,
            check: Duration::from_millis(100),
            report: Duration::from_millis(300),
        },
        ..SimConfig::default()
    }
}

#[test]
fn a_hung_port_times_out_its_check_and_never_holds_the_report() {
    let sim = Sim::new(limited());
    let app = api(&sim);
    sim.world.host().script(|s| {
        s.hang.insert(HostPort::AgentLogin);
    });
    let started = Instant::now();
    let r = get(&sim, &app, "/api/v1/diagnostics");
    assert!(
        started.elapsed() < Duration::from_millis(300),
        "{:?}",
        started.elapsed()
    );
    let login = check(&r.body, "agent.login");
    assert_eq!(login["status"], "fail");
    assert_eq!(login["code"], "check_timed_out");
    assert_eq!(login["params"], json!({ "timeout_ms": 100 }));
    assert_eq!(check(&r.body, "agent.binary")["status"], "pass");
}

#[test]
fn checks_still_queued_at_the_report_limit_time_out_too() {
    let mut cfg = limited();
    cfg.diagnostics.concurrency = 1;
    let sim = Sim::new(cfg);
    let app = api(&sim);
    sim.world.host().script(|s| {
        s.hang.extend([
            HostPort::Supervisor,
            HostPort::AgentCli,
            HostPort::AgentLogin,
            HostPort::ToolchainCache,
            HostPort::Releases,
        ]);
    });
    let started = Instant::now();
    let r = get(&sim, &app, "/api/v1/diagnostics");
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "{:?}",
        started.elapsed()
    );
    let timed_out: Vec<&Value> = r.body["checks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["code"] == "check_timed_out")
        .collect();
    assert_eq!(timed_out.len(), 5, "{}", r.body);
    assert!(
        timed_out.iter().any(|c| c["params"]["timeout_ms"] == 300),
        "a check that starts after the report limit names it: {}",
        r.body
    );
}

#[test]
fn findings_from_the_host_reach_the_report_with_their_fix() {
    let sim = Sim::new(SimConfig::default());
    let app = api(&sim);
    sim.world.host().script(|s| {
        s.login = AgentLogin::NotLoggedIn;
        s.failing_sources.insert(Tool::Node);
        s.cache.writable = false;
    });
    let r = get(&sim, &app, "/api/v1/diagnostics");
    let login = check(&r.body, "agent.login");
    assert_eq!(login["code"], "agent_not_logged_in");
    assert_eq!(
        login["fix"],
        json!({ "code": "run_agent_login", "params": { "command": "saka agent login" } })
    );
    let sources: Vec<&Value> = r.body["checks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["id"] == "toolchain.sources")
        .collect();
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0]["status"], "warn");
    assert_eq!(
        sources[0]["params"],
        json!({ "tool": "node", "url": "https://node.mirror.test/", "status": 500 })
    );
    assert_eq!(
        check(&r.body, "toolchain.cache")["code"],
        "toolchain_cache_unwritable"
    );
}

#[derive(Clone, Default)]
struct LogBuffer(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for LogBuffer {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn a_report_is_counted_in_metrics_and_logged() {
    let sim = Sim::new(SimConfig::default());
    let app = api(&sim);
    let logs = LogBuffer::default();
    let out = logs.clone();
    let subscriber = tracing_subscriber::fmt()
        .json()
        .with_max_level(tracing::Level::INFO)
        .with_writer(move || out.clone())
        .finish();
    tracing::subscriber::with_default(subscriber, || {
        get(&sim, &app, "/api/v1/diagnostics");
    });
    let text = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
    assert!(text.contains("diagnostics.ran"), "{text}");
    for field in [
        "\"pass\":10",
        "\"warn\":0",
        "\"fail\":1",
        "\"skip\":8",
        "\"duration_ms\":",
    ] {
        assert!(text.contains(field), "{field} in {text}");
    }
    assert!(text.contains("diagnostics.check_failed"), "{text}");
    assert!(text.contains("\"code\":\"server_not_ready\""), "{text}");
    let metrics = get(&sim, &app, "/metrics").text;
    for line in [
        "saka_diagnostic_checks_total{id=\"agent.login\",status=\"pass\"} 1",
        "saka_diagnostic_checks_total{id=\"server.ready\",status=\"fail\"} 1",
        "saka_diagnostic_checks_total{id=\"agent.quota\",status=\"skip\"} 1",
    ] {
        assert!(metrics.contains(line), "{line}\n{metrics}");
    }
}

fn all<'a>(body: &'a Value, id: &str) -> Vec<&'a Value> {
    body["checks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["id"] == id)
        .collect()
}

fn of<'a>(body: &'a Value, id: &str, label: &str) -> Vec<&'a Value> {
    all(body, id)
        .into_iter()
        .filter(|c| c["subject"]["label"] == label)
        .collect()
}

fn findings(checks: &[&Value]) -> Vec<(String, String)> {
    checks
        .iter()
        .map(|c| {
            (
                c["status"].as_str().unwrap().to_string(),
                c["code"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

fn pairs(rows: &[(&str, &str)]) -> Vec<(String, String)> {
    rows.iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect()
}

fn hook_deliveries(now: i64, failing: usize) -> Vec<HookDelivery> {
    (0..10)
        .map(|n| HookDelivery {
            delivered_at: Timestamp(now - (10 - n as i64) * 60_000),
            status_code: if n < failing { 500 } else { 202 },
        })
        .collect()
}

#[test]
fn app_installation_webhook_and_repo_findings_reach_the_report_with_their_fix() {
    let sim = Sim::new(SimConfig::default());
    let app = api(&sim);
    let now = saka_ports::Clock::now(&*sim.clock);
    sim.world
        .set_webhook_secret(b"0123456789abcdef0123456789abcdef");
    sim.world.set_repo_file(".nvmrc", Some("22\n"));
    sim.world.host().script(|s| {
        let mut partial = full_installation();
        partial
            .permissions
            .insert("pull_requests".into(), "read".into());
        s.installations.insert("acme/tasks".into(), partial);
        if let Ok(app) = &mut s.app {
            app.events.retain(|e| e != "status");
        }
        if let Ok(hook) = &mut s.webhook {
            hook.deliveries = hook_deliveries(now.0, 3);
        }
    });
    let r = get(&sim, &app, "/api/v1/diagnostics");
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let installation = of(&r.body, "scm.repo_access", "acme/tasks");
    assert_eq!(installation.len(), 1);
    assert_eq!(installation[0]["code"], "app_permissions_missing");
    assert_eq!(
        installation[0]["params"],
        json!({ "missing": [{ "permission": "pull_requests", "required": "write", "granted": "read" }] })
    );
    assert_eq!(
        installation[0]["fix"],
        json!({ "code": "grant_app_permissions", "params": { "url": "https://github.com/settings/apps/saka-sim/permissions" } })
    );
    let webhook = of(&r.body, "scm.webhook", "github");
    assert_eq!(
        findings(&webhook),
        pairs(&[
            ("warn", "app_events_missing"),
            ("warn", "webhook_deliveries_failing")
        ])
    );
    assert_eq!(webhook[1]["params"]["failed"], 3);
    assert_eq!(webhook[1]["params"]["total"], 10);
    assert!(all(&r.body, "scm.repo_webhook").is_empty());
    assert_eq!(
        webhook[1]["fix"],
        json!({ "code": "configure_app_webhook", "params": { "url": "/hooks/github" } })
    );
    let toolchain = of(&r.body, "toolchain.repo", "tasks/app");
    assert_eq!(
        findings(&toolchain),
        pairs(&[("fail", "toolchain_unpinned")])
    );
    assert_eq!(toolchain[0]["params"]["tool"], "node");
    assert_eq!(toolchain[0]["params"]["value"], "22");
    assert_eq!(
        toolchain[0]["fix"],
        json!({ "code": "run_saka_init", "params": { "command": "saka init" } }),
        "the fake index has no node 22, so there is no version to propose"
    );

    let delivery = saka_ports::WebhookDelivery {
        delivery_id: "d-1".into(),
        event: "ping".into(),
        action: None,
        repo_full_name: None,
        targets: Vec::new(),
    };
    sim.engine().accept_webhook("github", &delivery).unwrap();
    sim.world.host().script(|s| {
        s.installations.clear();
        if let Ok(hook) = &mut s.webhook {
            hook.deliveries = hook_deliveries(now.0, 0);
        }
        s.app = Err(saka_ports::PortError::permanent(
            "app_auth_failed",
            json!({ "status": 401, "app_id": 4242, "key_file": "/etc/saka/app.pem" }),
        ));
    });
    let r = get(&sim, &app, "/api/v1/diagnostics");
    for id in ["scm.credential", "scm.repo_access", "scm.webhook"] {
        let failed = all(&r.body, id)
            .into_iter()
            .filter(|c| c["subject"]["label"] != "ghes")
            .collect::<Vec<_>>();
        assert_eq!(
            findings(&failed),
            pairs(&[("fail", "app_auth_failed")]),
            "{id}"
        );
        assert_eq!(failed[0]["params"], json!({ "status": 401 }));
        assert_eq!(
            failed[0]["fix"],
            json!({ "code": "check_app_credentials", "params": { "app_id": 4242, "key_file": "/etc/saka/app.pem" } })
        );
    }
    sim.world
        .host()
        .script(|s| s.app = HostScript::default().app);
    let r = get(&sim, &app, "/api/v1/diagnostics");
    let webhook = of(&r.body, "scm.webhook", "github");
    assert_eq!(webhook[0]["code"], "webhook_ok");
    assert_eq!(
        webhook[0]["params"],
        json!({ "deliveries_24h": 10, "last_received_at": now.to_rfc3339() })
    );
    assert_eq!(
        of(&r.body, "scm.repo_access", "acme/tasks")[0]["code"],
        "app_installed"
    );
}

fn ping_for(delivery_id: &str, repo: &str) -> saka_ports::WebhookDelivery {
    saka_ports::WebhookDelivery {
        delivery_id: delivery_id.into(),
        event: "ping".into(),
        action: None,
        repo_full_name: Some(repo.into()),
        targets: Vec::new(),
    }
}

#[test]
fn a_token_integration_reports_its_credential_repo_access_and_repo_webhooks() {
    const DAY_MS: i64 = 86_400_000;
    let sim = Sim::new(SimConfig::default());
    add_web_project(&sim);
    sim.world
        .set_capabilities(saka_testkit::FAKE_TOKEN_CAPABILITIES);
    let app = api(&sim);
    let started = saka_ports::Clock::now(&*sim.clock);
    sim.world.host().script(|s| {
        s.token = Ok(TokenStatus {
            name: FAKE_TOKEN_NAME.into(),
            active: true,
            revoked: false,
            scopes: vec!["api".into()],
            expires_at: Some(started.plus_ms(5 * DAY_MS)),
        });
        s.members.insert(
            "acme/tasks".into(),
            RepoGrant::Member {
                access_level: Some(30),
            },
        );
        s.members.insert("acme/web".into(), RepoGrant::NoAccess);
        s.hang.insert(HostPort::ScmRepoWebhook);
    });
    let r = get(&sim, &app, "/api/v1/diagnostics");
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(
        findings(&of(&r.body, "scm.credential", "github")),
        pairs(&[("warn", "token_expiring")])
    );
    assert_eq!(
        of(&r.body, "scm.credential", "github")[0]["params"],
        json!({
            "name": FAKE_TOKEN_NAME,
            "expires_at": started.plus_ms(5 * DAY_MS).to_rfc3339(),
        })
    );
    assert_eq!(
        findings(&of(&r.body, "scm.repo_access", "acme/tasks")),
        pairs(&[
            ("pass", "repo_access_ok"),
            ("warn", "repo_access_recommended")
        ])
    );
    assert_eq!(
        findings(&of(&r.body, "scm.repo_access", "acme/web")),
        pairs(&[("fail", "repo_access_missing")])
    );
    assert!(of(&r.body, "scm.webhook", "github").is_empty());
    for repo in ["acme/tasks", "acme/web"] {
        assert_eq!(
            findings(&of(&r.body, "scm.repo_webhook", repo)),
            pairs(&[("skip", "webhook_not_configured")]),
            "{repo}: no secret, so the provider is not called (it would hang)"
        );
    }

    sim.world
        .set_webhook_secret(b"0123456789abcdef0123456789abcdef");
    sim.world.host().script(|s| {
        s.hang.remove(&HostPort::ScmRepoWebhook);
        s.hooks
            .insert("acme/web".into(), RepoHooks::Unreadable { status: 403 });
    });
    sim.engine()
        .accept_webhook("github", &ping_for("d-1", "acme/tasks"))
        .unwrap();
    let first = saka_ports::Clock::now(&*sim.clock);
    sim.clock.advance(60_000);
    sim.engine()
        .accept_webhook("github", &ping_for("d-2", "acme/web"))
        .unwrap();
    let second = saka_ports::Clock::now(&*sim.clock);
    let r = get(&sim, &app, "/api/v1/diagnostics");
    let tasks = of(&r.body, "scm.repo_webhook", "acme/tasks");
    assert_eq!(findings(&tasks), pairs(&[("pass", "repo_webhook_ok")]));
    assert_eq!(
        tasks[0]["params"],
        json!({ "hook_id": saka_testkit::diagnostics::FAKE_HOOK_ID, "last_received_at": first.to_rfc3339() }),
        "the delivery of the other repo is not counted"
    );
    let web = of(&r.body, "scm.repo_webhook", "acme/web");
    assert_eq!(
        findings(&web),
        pairs(&[("warn", "repo_webhook_unreadable")])
    );
    assert_eq!(
        web[0]["params"],
        json!({ "status": 403, "last_received_at": second.to_rfc3339() })
    );

    sim.world.host().script(|s| {
        s.hooks.insert(
            "acme/tasks".into(),
            RepoHooks::Listed(vec![saka_core::diagnostics::RepoHook {
                events: vec!["merge_requests_events".into()],
                ..saka_hook("other")
            }]),
        );
    });
    let r = get(&sim, &app, "/api/v1/diagnostics?check=scm.repo_webhook");
    let tasks = of(&r.body, "scm.repo_webhook", "acme/tasks");
    assert_eq!(findings(&tasks), pairs(&[("fail", "repo_webhook_missing")]));
    assert_eq!(tasks[0]["params"], json!({ "path": "/hooks/github" }));
    assert_eq!(
        tasks[0]["fix"]["params"]["url"],
        "https://github.com/acme/tasks/-/hooks"
    );
}

fn add_web_project(sim: &Sim) -> saka_core::ids::ProjectId {
    let engine = sim.engine();
    let repo = |full_name: &str| saka_engine::NewRepo {
        repo_id: None,
        integration_id: "github".into(),
        provider: "github".into(),
        full_name: full_name.into(),
        url: format!("https://github.com/{full_name}"),
        default_branch: "main".into(),
    };
    let binding = |full_name: &str, name: &str, is_default: bool| saka_engine::NewBinding {
        repo: repo(full_name),
        name: name.into(),
        is_default,
        base_branch: "main".into(),
        path_scope: vec!["**".into()],
        autonomy: None,
    };
    let (id, _) = engine
        .register_project(
            &saka_engine::NewProject {
                name: "web".into(),
                workspace: repo("acme/tasks"),
                project_path: "web".into(),
                base_branch: "main".into(),
                config_commit: sim.world.base_head(),
                config_sha256: saka_testkit::ZERO_SHA256.into(),
                workflow: saka_core::workflow::Workflow::default(),
                hook_skip: Vec::new(),
                bindings: vec![
                    binding("acme/tasks", "api", true),
                    binding("acme/web", "site", false),
                ],
            },
            &engine.cause("human:operator:local"),
        )
        .unwrap();
    id
}

fn link_and_store_a_pat(sim: &mut Sim) -> String {
    let engine = sim.engine();
    let accounts = sim.world.accounts();
    accounts.set_device_flow(true);
    accounts.set_user(7, "octo", Some("Octo Cat"));
    let cause = engine.cause(saka_engine::OPERATOR);
    let started = sim
        .block_on(engine.start_user_link("github", &cause, None))
        .unwrap();
    accounts.answer(saka_testkit::DeviceAnswer::Approve);
    let auth = started.authorization.id;
    assert!(sim.run_until(8_000, |s| {
        s.engine()
            .user_link_authorization("github", saka_engine::OPERATOR, auth)
            .is_ok_and(|v| v.link.is_some())
    }));
    let now = saka_ports::Clock::now(&*sim.clock);
    let pat = sim
        .world
        .accounts()
        .issue_pat(Some(now.plus_ms(3 * 86_400_000)));
    sim.block_on(engine.set_user_token(
        sim.project_id,
        &saka_core::secret::SecretText::new(pat.clone()),
        &engine.cause("human:operator:local"),
        None,
    ))
    .unwrap();
    pat
}

#[test]
fn links_tokens_and_overlapping_scopes_are_reported_per_subject_and_scoped_by_project() {
    let mut sim = Sim::new(SimConfig::default());
    let web = add_web_project(&sim);
    link_and_store_a_pat(&mut sim);
    let app = api(&sim);
    let r = get(&sim, &app, "/api/v1/diagnostics");
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let link = of(&r.body, "scm.user_link", "github");
    assert_eq!(findings(&link), pairs(&[("pass", "user_link_active")]));
    assert_eq!(link[0]["params"]["login"], "octo");
    let token = of(&r.body, "project.user_token", "tasks");
    assert_eq!(findings(&token), pairs(&[("warn", "user_token_expiring")]));
    assert_eq!(token[0]["params"]["login"], "octo");
    assert_eq!(
        token[0]["fix"],
        json!({ "code": "replace_user_token", "params": { "command": "saka project user-token set tasks" } })
    );
    assert_eq!(
        findings(&of(&r.body, "project.user_token", "web")),
        pairs(&[("skip", "user_token_absent")])
    );
    let overlap = of(&r.body, "project.config", "tasks");
    assert_eq!(findings(&overlap), pairs(&[("warn", "path_scope_overlap")]));
    assert_eq!(
        overlap[0]["params"],
        json!({ "repo": "app", "other_project": "web", "other_repo": "api" })
    );
    assert_eq!(
        overlap[0]["fix"],
        json!({ "code": "narrow_path_scope", "params": { "repo": "app" } })
    );
    let installs: Vec<&str> = all(&r.body, "scm.repo_access")
        .iter()
        .map(|c| c["subject"]["label"].as_str().unwrap())
        .collect();
    assert_eq!(
        installs,
        ["acme/tasks", "acme/web"],
        "one check per repo even when two projects bind it"
    );
    let repos: Vec<&str> = all(&r.body, "toolchain.repo")
        .iter()
        .map(|c| c["subject"]["label"].as_str().unwrap())
        .collect();
    assert_eq!(repos, ["tasks/app", "web/api", "web/site"]);

    let scoped = get(
        &sim,
        &app,
        &format!("/api/v1/diagnostics?project_id={}", web.0),
    );
    let labels = |id: &str| -> Vec<String> {
        all(&scoped.body, id)
            .iter()
            .map(|c| c["subject"]["label"].as_str().unwrap().to_string())
            .collect()
    };
    assert_eq!(labels("toolchain.repo"), ["web/api", "web/site"]);
    assert_eq!(labels("project.config"), ["web"]);
    assert_eq!(labels("project.user_token"), ["web"]);
    assert_eq!(labels("scm.repo_access"), ["acme/tasks", "acme/web"]);
    assert_eq!(labels("scm.user_link"), ["ghes", "github"]);
    assert_eq!(labels("agent.login"), ["claude:default"]);
}

const SECRET_MARKERS: [&str; 5] = ["ghs_", "ghu_", "ghr_", "github_pat_", "-----BEGIN"];

#[test]
fn an_adapter_without_diagnostics_skips_its_scm_checks_and_no_report_holds_a_secret() {
    let mut sim = Sim::new(SimConfig::default());
    let pat = link_and_store_a_pat(&mut sim);
    assert!(pat.starts_with("github_pat_"));
    let issued = sim.world.accounts().issued_tokens();
    assert!(issued.iter().any(|t| t.starts_with("ghu_")));
    let app = api(&sim);
    let full = get(&sim, &app, "/api/v1/diagnostics");
    assert_eq!(full.status, StatusCode::OK);
    assert_eq!(
        findings(&of(&full.body, "scm.credential", "github")),
        pairs(&[("pass", "app_ok")])
    );

    sim.world.set_capabilities(saka_ports::ScmCapabilities {
        diagnostics: None,
        ..saka_testkit::FAKE_CAPABILITIES
    });
    let limited = get(&sim, &app, "/api/v1/diagnostics");
    for (id, label) in [
        ("scm.credential", "github"),
        ("scm.repo_access", "acme/tasks"),
        ("scm.webhook", "github"),
        ("scm.user_link", "github"),
    ] {
        let c = of(&limited.body, id, label);
        assert_eq!(c.len(), 1, "{id}");
        assert_eq!(c[0]["status"], "skip", "{id}");
        assert_eq!(c[0]["code"], "unsupported_by_adapter", "{id}");
        assert_eq!(c[0]["fix"], Value::Null);
    }
    assert_eq!(
        of(&limited.body, "project.user_token", "tasks")[0]["code"],
        "user_token_expiring",
        "project checks do not depend on the adapter"
    );
    for (name, text) in [("full", &full.text), ("limited", &limited.text)] {
        for marker in SECRET_MARKERS {
            assert!(!text.contains(marker), "{marker} in the {name} report");
        }
        for secret in issued.iter().chain([&pat]) {
            assert!(
                !text.contains(secret.as_str()),
                "a secret in the {name} report"
            );
        }
        assert!(
            !text.contains("users.noreply"),
            "an email in the {name} report"
        );
    }
}

fn auth_finding(auth_enabled: bool, passkeys: u32) -> (String, String) {
    let sim = Sim::new(SimConfig {
        auth_enabled,
        ..SimConfig::default()
    });
    for n in 0..passkeys {
        sim.engine()
            .store()
            .transact(|tx| {
                tx.execute(
                    "INSERT INTO credential (id, name, passkey, created_at) VALUES (?1, 'key', '{}', 1)",
                    [format!("00000000-0000-0000-0000-00000000a{n:03}")],
                )?;
                Ok(())
            })
            .unwrap();
    }
    let app = api(&sim);
    let r = get(&sim, &app, "/api/v1/diagnostics?check=auth.config");
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let found = check(&r.body, "auth.config");
    (
        found["status"].as_str().unwrap().to_string(),
        found["code"].as_str().unwrap().to_string(),
    )
}

#[test]
fn the_auth_check_skips_without_an_auth_section_fails_without_a_passkey_and_passes_with_one() {
    assert_eq!(
        auth_finding(false, 0),
        ("skip".into(), "auth_not_configured".into())
    );
    assert_eq!(
        auth_finding(false, 1),
        ("skip".into(), "auth_not_configured".into())
    );
    assert_eq!(
        auth_finding(true, 0),
        ("fail".into(), "auth_no_passkey".into())
    );
    assert_eq!(auth_finding(true, 1), ("pass".into(), "auth_ready".into()));
    let sim = Sim::new(SimConfig {
        auth_enabled: true,
        ..SimConfig::default()
    });
    let app = api(&sim);
    let r = get(&sim, &app, "/api/v1/diagnostics?check=auth.config");
    let found = check(&r.body, "auth.config");
    assert_eq!(found["fix"]["code"], "run_auth_setup");
    assert_eq!(found["fix"]["params"]["command"], "saka auth setup");
}
