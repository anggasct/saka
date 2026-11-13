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

fn histogram(t: &mut Text, name: &str, help: &str, h: &saka_engine::SecondsHistogram) {
    t.family(name, "histogram", help);
    let bucket = format!("{name}_bucket");
    for (bound, n) in &h.buckets {
        t.sample(&bucket, &[("le", &bound.to_string())], n);
    }
    t.sample(&bucket, &[("le", "+Inf")], h.count);
    t.sample(&format!("{name}_sum"), &[], h.sum);
    t.sample(&format!("{name}_count"), &[], h.count);
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
        "saka_merge_queue_turns_total",
        "counter",
        "Merge queue turns, by outcome.",
    );
    for (outcome, n) in &m.merge_turns {
        t.sample("saka_merge_queue_turns_total", &[("outcome", outcome)], n);
    }
    t.family(
        "saka_merge_dequeues_total",
        "counter",
        "Merge requests withdrawn before the merge, by reason (head_moved, not_ready).",
    );
    for (reason, n) in &m.merge_dequeues {
        t.sample("saka_merge_dequeues_total", &[("reason", reason)], n);
    }
    t.family(
        "saka_ci_reruns_total",
        "counter",
        "Re-runs of failed CI jobs, by outcome (passed, failed, unsupported).",
    );
    for (outcome, n) in &m.ci_reruns {
        t.sample("saka_ci_reruns_total", &[("outcome", outcome)], n);
    }
    t.family(
        "saka_permission_requests_total",
        "counter",
        "Permission requests of agent sessions, by tool and the outcome when decided (allowed, denied, pending).",
    );
    for (tool, outcome, n) in &m.permission_requests {
        t.sample(
            "saka_permission_requests_total",
            &[("tool", tool), ("outcome", outcome)],
            n,
        );
    }
    t.family(
        "saka_merge_requests_total",
        "counter",
        "Merge requests, by who made them (operator, autonomy, queue).",
    );
    for (by, n) in &m.merge_requests {
        t.sample("saka_merge_requests_total", &[("by", by)], n);
    }
    t.family(
        "saka_project_config_reloads_total",
        "counter",
        "Reloads of a project's saka.toml, by outcome.",
    );
    for (outcome, n) in &m.config_reloads {
        t.sample(
            "saka_project_config_reloads_total",
            &[("outcome", outcome)],
            n,
        );
    }
    t.family(
        "saka_project_config_checks_total",
        "counter",
        "Checks of a saka.toml before registration, by outcome.",
    );
    for (outcome, n) in &m.config_checks {
        t.sample(
            "saka_project_config_checks_total",
            &[("outcome", outcome)],
            n,
        );
    }
    t.family(
        "saka_diagnostic_checks_total",
        "counter",
        "Diagnostic check results, by check id and status.",
    );
    for (id, status, n) in &m.diagnostic_checks {
        t.sample(
            "saka_diagnostic_checks_total",
            &[("id", id), ("status", status)],
            n,
        );
    }
    t.family(
        "saka_chat_actions_total",
        "counter",
        "Button presses from a chat, by platform and outcome.",
    );
    for (platform, outcome, n) in &m.chat_actions {
        t.sample(
            "saka_chat_actions_total",
            &[("platform", platform), ("outcome", outcome)],
            n,
        );
    }
    t.family(
        "saka_path_scope_violations_total",
        "counter",
        "Task diffs found outside their allowed paths, by the stage that checked them.",
    );
    for (stage, n) in &m.path_scope_violations {
        t.sample("saka_path_scope_violations_total", &[("stage", stage)], n);
    }
    t.family(
        "saka_docs_changes_total",
        "counter",
        "Docs pull requests that finished, by whether Saka merged them or they were closed and skipped.",
    );
    for (outcome, n) in &m.docs_changes {
        t.sample("saka_docs_changes_total", &[("outcome", outcome)], n);
    }
    t.family(
        "saka_deployments_total",
        "counter",
        "Deployments that finished, by environment and outcome.",
    );
    for (environment, outcome, n) in &m.deployments {
        t.sample(
            "saka_deployments_total",
            &[("environment", environment), ("outcome", outcome)],
            n,
        );
    }
    t.family(
        "saka_deploy_checks_total",
        "counter",
        "Deployment checks, by phase and state.",
    );
    for (phase, state, n) in &m.deploy_checks {
        t.sample(
            "saka_deploy_checks_total",
            &[("phase", phase), ("state", state)],
            n,
        );
    }
    t.family(
        "saka_reverts_total",
        "counter",
        "Revert tasks, by how they ended: merged, stopped on a conflict, or cancelled.",
    );
    for (outcome, n) in &m.reverts {
        t.sample("saka_reverts_total", &[("outcome", outcome)], n);
    }
    t.family(
        "saka_quick_specs_total",
        "counter",
        "Quick tasks, by kind and outcome (created, approved, not_approved, refused).",
    );
    for (kind, outcome, n) in &m.quick_specs {
        t.sample(
            "saka_quick_specs_total",
            &[("kind", kind), ("outcome", outcome)],
            n,
        );
    }
    t.family(
        "saka_issue_intakes_total",
        "counter",
        "Issue intakes accepted, by lane, kind and what started them.",
    );
    for (lane, kind, cause, n) in &m.issue_intakes {
        t.sample(
            "saka_issue_intakes_total",
            &[("lane", lane), ("kind", kind), ("cause", cause)],
            n,
        );
    }
    t.family(
        "saka_issue_intake_denied_total",
        "counter",
        "Issue triggers from users without write access that were ignored.",
    );
    t.sample("saka_issue_intake_denied_total", &[], m.issue_denied);
    t.family(
        "saka_issue_comments_total",
        "counter",
        "Status comments on issues, by outcome (posted, failed).",
    );
    for (outcome, n) in &m.issue_comments {
        t.sample("saka_issue_comments_total", &[("outcome", outcome)], n);
    }
    t.family(
        "saka_issue_close_total",
        "counter",
        "Issues Saka closed after their work merged, by outcome (closed, already_closed, failed).",
    );
    for (outcome, n) in &m.issue_closes {
        t.sample("saka_issue_close_total", &[("outcome", outcome)], n);
    }
    t.family(
        "saka_user_link_authorizations_total",
        "counter",
        "Device-flow authorizations that link an SCM account, by integration and the state reached.",
    );
    for (integration, state, n) in &m.user_link_authorizations {
        t.sample(
            "saka_user_link_authorizations_total",
            &[("integration", integration), ("state", state)],
            n,
        );
    }
    t.family(
        "saka_user_link_refresh_total",
        "counter",
        "Refreshes of linked SCM account tokens, by integration and outcome.",
    );
    for (integration, outcome, n) in &m.user_link_refreshes {
        t.sample(
            "saka_user_link_refresh_total",
            &[("integration", integration), ("outcome", outcome)],
            n,
        );
    }
    t.family(
        "saka_change_requests_opened_total",
        "counter",
        "Change requests opened, by the credential that opened them.",
    );
    for (credential, n) in &m.change_requests_opened {
        t.sample(
            "saka_change_requests_opened_total",
            &[("credential", credential)],
            n,
        );
    }
    t.family(
        "saka_intake_changes_total",
        "counter",
        "Pull requests seen by intake scans, by outcome.",
    );
    for (outcome, n) in &m.intake_changes {
        t.sample("saka_intake_changes_total", &[("outcome", outcome)], n);
    }
    t.family(
        "saka_architect_code_checkouts_total",
        "counter",
        "Code repo checkouts prepared for architect drafts, by outcome.",
    );
    for (outcome, n) in &m.architect_code_checkouts {
        t.sample(
            "saka_architect_code_checkouts_total",
            &[("outcome", outcome)],
            n,
        );
    }
    t.family(
        "saka_review_requests_total",
        "counter",
        "Review requests created, by what asked for them.",
    );
    for (cause, n) in &m.review_requests {
        t.sample("saka_review_requests_total", &[("cause", cause)], n);
    }
    t.family(
        "saka_review_verdicts_total",
        "counter",
        "Reviewer verdicts recorded, by outcome and what they were judged against.",
    );
    for (outcome, basis, n) in &m.review_verdicts {
        t.sample(
            "saka_review_verdicts_total",
            &[("outcome", outcome), ("basis", basis)],
            n,
        );
    }
    t.family(
        "saka_review_format_rejected_total",
        "counter",
        "Reviewer replies rejected for the verdict format, by reason.",
    );
    for (code, n) in &m.review_format_rejected {
        t.sample("saka_review_format_rejected_total", &[("code", code)], n);
    }
    t.family(
        "saka_fix_rounds_total",
        "counter",
        "Fix rounds started, by what asked for them.",
    );
    for (cause, n) in &m.fix_rounds {
        t.sample("saka_fix_rounds_total", &[("cause", cause)], n);
    }
    t.family(
        "saka_fix_exhausted_total",
        "counter",
        "Tasks stopped because their fix rounds ran out.",
    );
    t.sample("saka_fix_exhausted_total", &[], m.fix_exhausted);
    t.family(
        "saka_regression_checks_total",
        "counter",
        "Regression checks of bugfix tasks, by outcome.",
    );
    for (outcome, n) in &m.regression_checks {
        t.sample("saka_regression_checks_total", &[("outcome", outcome)], n);
    }
    t.family(
        "saka_review_posts_total",
        "counter",
        "Native reviews posted, by review event.",
    );
    for (event, n) in &m.review_posts {
        t.sample("saka_review_posts_total", &[("event", event)], n);
    }
    histogram(
        &mut t,
        "saka_intake_scan_seconds",
        "Time one intake scan of a repo takes.",
        &m.intake_scan_seconds,
    );
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
        "saka_incident_deliveries_total",
        "counter",
        "Incident source deliveries by source and outcome (accepted, duplicate, ignored, rejected).",
    );
    for (source, outcome, n) in &m.incident_deliveries {
        t.sample(
            "saka_incident_deliveries_total",
            &[("source", source), ("outcome", outcome)],
            n,
        );
    }
    t.family(
        "saka_incident_signals_total",
        "counter",
        "Incident signals by source and outcome (opened, updated, resolved, ignored, limited, truncated).",
    );
    for (source, outcome, n) in &m.incident_signals {
        t.sample(
            "saka_incident_signals_total",
            &[("source", source), ("outcome", outcome)],
            n,
        );
    }
    histogram(
        &mut t,
        "saka_gate_duration_seconds",
        "Duration of Saka gate runs.",
        &m.gate_seconds,
    );
    t.family(
        "saka_dod_verifications_total",
        "counter",
        "Definition of Done checks after a merge, by outcome.",
    );
    for (outcome, n) in &m.dod_verifications {
        t.sample("saka_dod_verifications_total", &[("outcome", outcome)], n);
    }
    t.family(
        "saka_spec_drafts_active",
        "gauge",
        "Spec drafts preparing, drafting, or committing.",
    );
    t.sample("saka_spec_drafts_active", &[], m.spec_drafts_active);
    t.family(
        "saka_spec_drafts_finished_total",
        "counter",
        "Spec drafts that ended, by state.",
    );
    for (state, n) in &m.spec_drafts_finished {
        t.sample("saka_spec_drafts_finished_total", &[("state", state)], n);
    }
    t.family(
        "saka_spec_draft_lint_rounds_total",
        "counter",
        "Lint rounds on architect drafts, by outcome.",
    );
    for (outcome, n) in &m.spec_draft_lint_rounds {
        t.sample(
            "saka_spec_draft_lint_rounds_total",
            &[("outcome", outcome)],
            n,
        );
    }
    histogram(
        &mut t,
        "saka_step_advance_seconds",
        "Time from a step's merge to the admission of the step waiting on it.",
        &m.advance_seconds,
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
        "saka_hook_runs_total",
        "counter",
        "Hook runs by action, mode, and outcome.",
    );
    for (action, mode, outcome, n) in &m.hook_runs {
        t.sample(
            "saka_hook_runs_total",
            &[("action", action), ("mode", mode), ("outcome", outcome)],
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
