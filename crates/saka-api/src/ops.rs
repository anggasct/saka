use std::collections::BTreeMap;
use std::fmt::Write as _;

use axum::extract::State;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};

use crate::dto;
use crate::http::ok;
use crate::problem::ApiError;
use crate::state::AppState;

#[utoipa::path(get, path = "/healthz", tag = "ops", responses((status = 200, body = dto::Health)))]
pub async fn healthz() -> Response {
    ok(
        StatusCode::OK,
        &dto::Health {
            status: "ok".into(),
        },
    )
}

#[utoipa::path(
    get, path = "/readyz", tag = "ops",
    responses((status = 200, body = dto::Ready), (status = 503, body = dto::Ready))
)]
pub async fn readyz(State(s): State<AppState>) -> Response {
    let r = s.engine_if_ready().map(|e| e.readiness());
    let word = |ok: bool| if ok { "ok" } else { "failing" }.to_string();
    let checks = dto::ReadyChecks {
        db: word(r.is_some_and(|r| r.db)),
        migrations: word(r.is_some_and(|r| r.migrations)),
        worker: word(r.is_some_and(|r| r.worker)),
        reconciler: word(r.is_some_and(|r| r.reconciler && r.sessions_reattached)),
    };
    let ready = r
        .is_some_and(|r| r.db && r.migrations && r.worker && r.reconciler && r.sessions_reattached);
    ok(
        if ready {
            StatusCode::OK
        } else {
            StatusCode::SERVICE_UNAVAILABLE
        },
        &dto::Ready {
            status: if ready { "ready" } else { "not_ready" }.into(),
            checks,
        },
    )
}

const BUCKETS: &[f64] = &[1.0, 10.0, 60.0, 300.0, 900.0, 1800.0, 3600.0, 7200.0];

fn label(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

struct Text(String);

impl Text {
    fn family(&mut self, name: &str, kind: &str, help: &str) {
        let _ = writeln!(self.0, "# HELP {name} {help}");
        let _ = writeln!(self.0, "# TYPE {name} {kind}");
    }

    fn sample(&mut self, name: &str, labels: &[(&str, &str)], value: impl std::fmt::Display) {
        if labels.is_empty() {
            let _ = writeln!(self.0, "{name} {value}");
        } else {
            let l: Vec<String> = labels
                .iter()
                .map(|(k, v)| format!("{k}=\"{}\"", label(v)))
                .collect();
            let _ = writeln!(self.0, "{name}{{{}}} {value}", l.join(","));
        }
    }
}

#[utoipa::path(
    get, path = "/metrics", tag = "ops",
    responses((status = 200, content_type = "text/plain; version=0.0.4", body = String))
)]
pub async fn metrics(State(s): State<AppState>) -> Response {
    let engine = match s.engine() {
        Ok(e) => e,
        Err(e) => return e.into_response(),
    };
    let m = match engine.metrics() {
        Ok(m) => m,
        Err(e) => return ApiError::from(e).into_response(),
    };
    let mut t = Text(String::new());
    t.family("saka_build_info", "gauge", "Always 1.");
    t.sample(
        "saka_build_info",
        &[("version", &s.config().server_version)],
        1,
    );

    t.family("saka_jobs", "gauge", "Jobs per kind and state.");
    for (kind, state, n) in &m.jobs {
        t.sample("saka_jobs", &[("kind", kind), ("state", state)], n);
    }
    t.family(
        "saka_job_oldest_ready_age_seconds",
        "gauge",
        "Age of the oldest queued job that is due.",
    );
    let age = m.oldest_ready_job_age_ms.unwrap_or(0) as f64 / 1000.0;
    t.sample("saka_job_oldest_ready_age_seconds", &[], age);

    t.family(
        "saka_job_runs_total",
        "counter",
        "Job runs per kind and outcome.",
    );
    for (kind, outcome, n) in &m.job_runs {
        t.sample(
            "saka_job_runs_total",
            &[("kind", kind), ("outcome", outcome)],
            n,
        );
    }
    t.family(
        "saka_sessions_active",
        "gauge",
        "Sessions not in a terminal state.",
    );
    for (state, n) in &m.sessions_active {
        t.sample("saka_sessions_active", &[("state", state)], n);
    }
    t.family(
        "saka_permission_requests_pending",
        "gauge",
        "Permission requests waiting for a person.",
    );
    t.sample(
        "saka_permission_requests_pending",
        &[],
        m.permission_requests_pending,
    );
    t.family(
        "saka_environments_teardown_failed",
        "gauge",
        "Environments whose teardown ran out of attempts and wait for cleanup.",
    );
    t.sample(
        "saka_environments_teardown_failed",
        &[],
        m.environments_teardown_failed,
    );

    t.family(
        "saka_sessions_resumed_total",
        "counter",
        "Sessions that continue an earlier one, by cause.",
    );
    for (cause, n) in &m.sessions_resumed {
        t.sample("saka_sessions_resumed_total", &[("cause", cause)], n);
    }
    t.family(
        "saka_budget_exhausted_total",
        "counter",
        "Stage runs stopped by a budget, by dimension.",
    );
    for (dimension, n) in &m.budget_exhausted {
        t.sample(
            "saka_budget_exhausted_total",
            &[("dimension", dimension)],
            n,
        );
    }
    t.family(
        "saka_usage_tokens_total",
        "counter",
        "Input and output tokens reported by the agent CLI.",
    );
    for (account, tokens, _) in &m.usage {
        t.sample("saka_usage_tokens_total", &[("account", account)], tokens);
    }
    t.family(
        "saka_usage_cost_usd_micros_total",
        "counter",
        "API-equivalent cost reported by the agent CLI, in micro-dollars.",
    );
    for (account, _, micros) in &m.usage {
        t.sample(
            "saka_usage_cost_usd_micros_total",
            &[("account", account)],
            micros,
        );
    }
    t.family(
        "saka_account_utilization",
        "gauge",
        "Last observed utilization of an agent account rate limit window.",
    );
    for (account, window, utilization) in &m.account_utilization {
        t.sample(
            "saka_account_utilization",
            &[("account", account), ("window", window)],
            utilization,
        );
    }

    t.family("saka_work_items", "gauge", "Work items per state.");
    for (state, n) in &m.work_items {
        t.sample("saka_work_items", &[("state", state)], n);
    }
    t.family(
        "saka_admission_waiting_tasks",
        "gauge",
        "Queued tasks held back by a step or spec they depend on.",
    );
    for (reason, n) in &m.admission_waiting {
        t.sample("saka_admission_waiting_tasks", &[("reason", reason)], n);
    }

    t.family(
        "saka_auto_retries_total",
        "counter",
        "Blockers retried automatically, by code.",
    );
    for (code, n) in &m.auto_retries {
        t.sample("saka_auto_retries_total", &[("code", code)], n);
    }
    t.family(
        "saka_gate_runs_total",
        "counter",
        "Gate runs by source and outcome.",
    );
    for (source, outcome, n) in &m.gate_runs {
        t.sample(
            "saka_gate_runs_total",
            &[("source", source), ("outcome", outcome)],
            n,
        );
    }
    t.family(
        "saka_gate_duration_seconds",
        "histogram",
        "Duration of Saka gate runs.",
    );
    for (bound, n) in &m.gate_seconds.buckets {
        let le = bound.to_string();
        t.sample("saka_gate_duration_seconds_bucket", &[("le", &le)], n);
    }
    t.sample(
        "saka_gate_duration_seconds_bucket",
        &[("le", "+Inf")],
        m.gate_seconds.count,
    );
    t.sample("saka_gate_duration_seconds_sum", &[], m.gate_seconds.sum);
    t.sample(
        "saka_gate_duration_seconds_count",
        &[],
        m.gate_seconds.count,
    );

    t.family(
        "saka_webhook_deliveries_total",
        "counter",
        "Webhook deliveries by integration, event, and outcome.",
    );
    for (integration, event, outcome, n) in &m.webhook_deliveries {
        t.sample(
            "saka_webhook_deliveries_total",
            &[
                ("integration", integration),
                ("event", event),
                ("outcome", outcome),
            ],
            n,
        );
    }

    t.family(
        "saka_work_packet_bytes",
        "histogram",
        "Size of work packets sent as the first turn of a session.",
    );
    let packets = &m.work_packet_bytes;
    for (bound, n) in &packets.buckets {
        let le = bound.to_string();
        t.sample("saka_work_packet_bytes_bucket", &[("le", &le)], n);
    }
    t.sample(
        "saka_work_packet_bytes_bucket",
        &[("le", "+Inf")],
        packets.count,
    );
    t.sample("saka_work_packet_bytes_sum", &[], packets.sum);
    t.sample("saka_work_packet_bytes_count", &[], packets.count);

    t.family(
        "saka_stage_duration_seconds",
        "histogram",
        "Duration of finished stage runs.",
    );
    let mut groups: BTreeMap<(&str, &str), Vec<f64>> = BTreeMap::new();
    for (stage, outcome, ms) in &m.stage_durations {
        groups
            .entry((stage.as_str(), outcome.as_str()))
            .or_default()
            .push(*ms as f64 / 1000.0);
    }
    for ((stage, outcome), secs) in &groups {
        for bound in BUCKETS {
            let n = secs.iter().filter(|s| **s <= *bound).count();
            let le = bound.to_string();
            t.sample(
                "saka_stage_duration_seconds_bucket",
                &[("stage", stage), ("outcome", outcome), ("le", &le)],
                n,
            );
        }
        let labels = [("stage", *stage), ("outcome", *outcome)];
        let mut inf = labels.to_vec();
        inf.push(("le", "+Inf"));
        t.sample("saka_stage_duration_seconds_bucket", &inf, secs.len());
        t.sample(
            "saka_stage_duration_seconds_sum",
            &labels,
            secs.iter().sum::<f64>(),
        );
        t.sample("saka_stage_duration_seconds_count", &labels, secs.len());
    }

    t.family(
        "saka_adapter_requests_total",
        "counter",
        "Adapter calls per adapter and outcome.",
    );
    for (adapter, outcome, n) in &m.adapter_requests {
        t.sample(
            "saka_adapter_requests_total",
            &[("adapter", adapter), ("outcome", outcome)],
            n,
        );
    }
    t.family(
        "saka_http_requests_total",
        "counter",
        "API requests per route and status.",
    );
    for (route, status, n) in s.http_counts() {
        let status = status.to_string();
        t.sample(
            "saka_http_requests_total",
            &[("route", &route), ("status", &status)],
            n,
        );
    }

    let mut response = (StatusCode::OK, t.0).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; version=0.0.4"),
    );
    response
}
