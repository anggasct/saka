use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use saka_core::admission::Waiting;
use saka_core::event::Event;
use saka_core::gate::GatePolicy;
use saka_core::ids::WorkItemId;
use saka_core::job::JobKind;
use saka_core::model::Task;
use saka_core::vocab::{BlockerCode, GateSource, Stage, StagePhase};
use saka_store::list;

use crate::worker::JobResult;
use crate::{Engine, Result};

#[derive(Debug, Default)]
pub struct Counters {
    job_runs: Mutex<BTreeMap<(String, String), u64>>,
    adapter_requests: Mutex<BTreeMap<(String, String), u64>>,
    waiting: Mutex<BTreeMap<WorkItemId, Vec<Waiting>>>,
    packet_bytes: Mutex<Vec<u64>>,
    webhooks: Mutex<BTreeMap<(String, String, String), u64>>,
    hook_runs: Mutex<BTreeMap<(String, String, String), u64>>,
    gate_runs: Mutex<BTreeMap<(String, String), u64>>,
    auto_retries: Mutex<BTreeMap<(String, String), u64>>,
    merge_turns: Mutex<BTreeMap<(String, String), u64>>,
    merge_dequeues: Mutex<BTreeMap<(String, String), u64>>,
    merge_requests: Mutex<BTreeMap<(String, String), u64>>,
    gate_seconds: Mutex<SecondsHistogram>,
    advance_seconds: Mutex<SecondsHistogram>,
    dod: Mutex<BTreeMap<(String, String), u64>>,
    drafts_finished: Mutex<BTreeMap<(String, String), u64>>,
    draft_lint_rounds: Mutex<BTreeMap<(String, String), u64>>,
    config_reloads: Mutex<BTreeMap<(String, String), u64>>,
    config_checks: Mutex<BTreeMap<(String, String), u64>>,
    path_scope_violations: Mutex<BTreeMap<(String, String), u64>>,
    docs_changes: Mutex<BTreeMap<(String, String), u64>>,
    quick_specs: Mutex<BTreeMap<(String, String), u64>>,
    user_link_authorizations: Mutex<BTreeMap<(String, String), u64>>,
    user_link_refreshes: Mutex<BTreeMap<(String, String), u64>>,
    change_requests_opened: Mutex<BTreeMap<(String, String), u64>>,
    diagnostic_checks: Mutex<BTreeMap<(String, String), u64>>,
    intake_changes: Mutex<BTreeMap<(String, String), u64>>,
    chat_actions: Mutex<BTreeMap<(String, String), u64>>,
    review_requests: Mutex<BTreeMap<(String, String), u64>>,
    review_verdicts: Mutex<BTreeMap<(String, String), u64>>,
    review_format_rejected: Mutex<BTreeMap<(String, String), u64>>,
    review_posts: Mutex<BTreeMap<(String, String), u64>>,
    fix_rounds: Mutex<BTreeMap<(String, String), u64>>,
    fix_exhausted: Mutex<BTreeMap<(String, String), u64>>,
    regression_checks: Mutex<BTreeMap<(String, String), u64>>,
    intake_scan_seconds: Mutex<SecondsHistogram>,
    pub(crate) worker_started: AtomicBool,
    pub(crate) reconciler_started: AtomicBool,
}

fn bump(map: &Mutex<BTreeMap<(String, String), u64>>, a: &str, b: &str) {
    if let Ok(mut m) = map.lock() {
        *m.entry((a.to_string(), b.to_string())).or_default() += 1;
    }
}

fn copy(map: &Mutex<BTreeMap<(String, String), u64>>) -> Vec<(String, String, u64)> {
    map.lock()
        .map(|m| {
            m.iter()
                .map(|((a, b), n)| (a.clone(), b.clone(), *n))
                .collect()
        })
        .unwrap_or_default()
}

fn adapter(kind: JobKind) -> &'static str {
    let name = kind.as_str();
    if name.starts_with("scm.") {
        "github"
    } else if name.starts_with("session.") {
        "claude"
    } else {
        "git"
    }
}

pub const GATE_SECONDS_BUCKETS: [f64; 7] = [1.0, 10.0, 60.0, 300.0, 900.0, 1800.0, 3600.0];
pub const ADVANCE_SECONDS_BUCKETS: [f64; 8] = [5.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0, 1800.0];

#[derive(Debug, Clone, PartialEq, Default)]
pub struct SecondsHistogram {
    pub buckets: Vec<(f64, u64)>,
    pub count: u64,
    pub sum: f64,
}

fn observe(histogram: &Mutex<SecondsHistogram>, bounds: &[f64], seconds: f64) {
    if let Ok(mut h) = histogram.lock() {
        if h.buckets.is_empty() {
            h.buckets = bounds.iter().map(|b| (*b, 0)).collect();
        }
        for (bound, n) in &mut h.buckets {
            if seconds <= *bound {
                *n += 1;
            }
        }
        h.count += 1;
        h.sum += seconds;
    }
}

fn snapshot_of(histogram: &Mutex<SecondsHistogram>) -> SecondsHistogram {
    histogram.lock().map(|h| h.clone()).unwrap_or_default()
}

pub const PACKET_BYTE_BUCKETS: [u64; 6] = [4_096, 16_384, 65_536, 131_072, 262_144, 524_288];

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PacketBytes {
    pub buckets: Vec<(u64, u64)>,
    pub count: u64,
    pub sum: u64,
}

impl Counters {
    pub(crate) fn auto_retry(&self, code: &str) {
        bump(&self.auto_retries, code, "");
    }

    pub(crate) fn merge_turn(&self, outcome: &str) {
        bump(&self.merge_turns, outcome, "");
    }

    pub(crate) fn merge_dequeue(&self, reason: &str) {
        bump(&self.merge_dequeues, reason, "");
    }

    pub(crate) fn merge_request(&self, by: &str) {
        bump(&self.merge_requests, by, "");
    }

    pub(crate) fn gate_run(&self, source: &str, outcome: &str, seconds: f64) {
        bump(&self.gate_runs, source, outcome);
        observe(&self.gate_seconds, &GATE_SECONDS_BUCKETS, seconds);
    }

    pub(crate) fn draft_finished(&self, state: &str) {
        bump(&self.drafts_finished, state, "");
    }

    pub(crate) fn draft_lint_round(&self, outcome: &str) {
        bump(&self.draft_lint_rounds, outcome, "");
    }

    pub(crate) fn config_reload(&self, outcome: &str) {
        bump(&self.config_reloads, outcome, "");
    }

    pub(crate) fn config_check(&self, outcome: &str) {
        bump(&self.config_checks, outcome, "");
    }

    pub(crate) fn user_link_authorization(&self, integration: &str, state: &str) {
        bump(&self.user_link_authorizations, integration, state);
    }

    pub(crate) fn user_link_refresh(&self, integration: &str, outcome: &str) {
        bump(&self.user_link_refreshes, integration, outcome);
    }

    pub(crate) fn change_request_opened(&self, credential: &str) {
        bump(&self.change_requests_opened, credential, "");
    }

    pub(crate) fn diagnostic_check(&self, id: &str, status: &str) {
        bump(&self.diagnostic_checks, id, status);
    }

    pub(crate) fn path_scope_violation(&self, stage: &str) {
        bump(&self.path_scope_violations, stage, "");
    }

    pub(crate) fn docs_change(&self, outcome: &str) {
        bump(&self.docs_changes, outcome, "");
    }

    pub(crate) fn quick_spec(&self, kind: &str, outcome: &str) {
        bump(&self.quick_specs, kind, outcome);
    }

    pub(crate) fn dod(&self, outcome: &str) {
        bump(&self.dod, outcome, "");
    }

    pub(crate) fn chat_action(&self, platform: &str, outcome: &str) {
        bump(&self.chat_actions, platform, outcome);
    }

    pub(crate) fn intake_change(&self, outcome: &str) {
        bump(&self.intake_changes, outcome, "");
    }

    pub(crate) fn review_verdict(&self, outcome: &str, basis: &str) {
        bump(&self.review_verdicts, outcome, basis);
    }

    pub(crate) fn regression_check(&self, outcome: &str) {
        bump(&self.regression_checks, outcome, "");
    }

    pub(crate) fn review_format_rejected(&self, code: &str) {
        bump(&self.review_format_rejected, code, "");
    }

    pub(crate) fn review_post(&self, event: &str) {
        bump(&self.review_posts, event, "");
    }

    pub(crate) fn fix_round(&self, cause: &str) {
        bump(&self.fix_rounds, cause, "");
    }

    pub(crate) fn fix_exhausted(&self) {
        bump(&self.fix_exhausted, "", "");
    }

    pub(crate) fn review_request(&self, cause: &str) {
        bump(&self.review_requests, cause, "");
    }

    pub(crate) fn intake_scan(&self, seconds: f64) {
        observe(&self.intake_scan_seconds, &ADVANCE_SECONDS_BUCKETS, seconds);
    }

    pub(crate) fn step_advance(&self, seconds: f64) {
        observe(&self.advance_seconds, &ADVANCE_SECONDS_BUCKETS, seconds);
    }

    pub(crate) fn webhook(&self, integration: &str, event: &str, outcome: &str) {
        if let Ok(mut m) = self.webhooks.lock() {
            *m.entry((
                integration.to_string(),
                event.to_string(),
                outcome.to_string(),
            ))
            .or_default() += 1;
        }
    }

    pub(crate) fn hook_run(&self, action: &str, mode: &str, outcome: &str) {
        if let Ok(mut m) = self.hook_runs.lock() {
            *m.entry((action.to_string(), mode.to_string(), outcome.to_string()))
                .or_default() += 1;
        }
    }

    fn hook_run_counts(&self) -> Vec<(String, String, String, u64)> {
        self.hook_runs
            .lock()
            .map(|m| {
                m.iter()
                    .map(|((a, mo, o), n)| (a.clone(), mo.clone(), o.clone(), *n))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn webhook_counts(&self) -> Vec<(String, String, String, u64)> {
        self.webhooks
            .lock()
            .map(|m| {
                m.iter()
                    .map(|((i, e, o), n)| (i.clone(), e.clone(), o.clone(), *n))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub(crate) fn work_packet(&self, bytes: u64) {
        if let Ok(mut counts) = self.packet_bytes.lock() {
            if counts.is_empty() {
                counts.resize(PACKET_BYTE_BUCKETS.len() + 2, 0);
            }
            for (slot, bound) in counts.iter_mut().zip(PACKET_BYTE_BUCKETS) {
                if bytes <= bound {
                    *slot += 1;
                }
            }
            let n = PACKET_BYTE_BUCKETS.len();
            counts[n] += 1;
            counts[n + 1] = counts[n + 1].saturating_add(bytes);
        }
    }

    fn packet_bytes(&self) -> PacketBytes {
        let counts = self
            .packet_bytes
            .lock()
            .map(|c| c.clone())
            .unwrap_or_default();
        let n = PACKET_BYTE_BUCKETS.len();
        PacketBytes {
            buckets: PACKET_BYTE_BUCKETS
                .iter()
                .enumerate()
                .map(|(i, bound)| (*bound, counts.get(i).copied().unwrap_or(0)))
                .collect(),
            count: counts.get(n).copied().unwrap_or(0),
            sum: counts.get(n + 1).copied().unwrap_or(0),
        }
    }

    pub(crate) fn job(&self, kind: JobKind, result: &JobResult, adapter_ok: Option<bool>) {
        let outcome = match result {
            JobResult::Retrying { .. } => "retry",
            JobResult::Exhausted { .. } => "exhausted",
            _ => "succeeded",
        };
        bump(&self.job_runs, kind.as_str(), outcome);
        if let Some(ok) = adapter_ok {
            bump(
                &self.adapter_requests,
                adapter(kind),
                if ok { "ok" } else { "error" },
            );
        }
    }
}

impl Counters {
    pub(crate) fn note_waiting(&self, current: Vec<(WorkItemId, Vec<Waiting>)>) {
        let Ok(mut waiting) = self.waiting.lock() else {
            return;
        };
        let current: BTreeMap<WorkItemId, Vec<Waiting>> = current.into_iter().collect();
        for (id, waits) in &current {
            if waiting.get(id) == Some(waits) {
                continue;
            }
            let kind = waits.first().map_or("", Waiting::kind);
            let depends_on: Vec<String> = waits
                .iter()
                .map(|w| match w {
                    Waiting::Step { work_item_id, .. } => work_item_id.to_string(),
                    Waiting::Spec { spec_id, .. } => spec_id.to_string(),
                    Waiting::Quota {
                        account, window, ..
                    } => format!("{account}:{window}"),
                    Waiting::PortBlock => "port_block".to_string(),
                    Waiting::ArchitectSlot => "architect_slot".to_string(),
                })
                .collect();
            tracing::info!(
                work_item_id = %id,
                waiting_on_kind = kind,
                depends_on = %depends_on.join(","),
                "task waits for its dependencies"
            );
        }
        *waiting = current;
    }

    fn waiting_counts(&self) -> Vec<(String, i64)> {
        let waiting = self.waiting.lock().map(|w| w.clone()).unwrap_or_default();
        ["spec", "step"]
            .into_iter()
            .map(|reason| {
                let n = waiting
                    .values()
                    .filter(|waits| waits.iter().any(|w| w.kind() == reason))
                    .count();
                (reason.to_string(), i64::try_from(n).unwrap_or(i64::MAX))
            })
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct MetricsSnapshot {
    pub jobs: Vec<(String, String, i64)>,
    pub oldest_ready_job_age_ms: Option<i64>,
    pub job_runs: Vec<(String, String, u64)>,
    pub sessions_active: Vec<(String, i64)>,
    pub permission_requests_pending: i64,
    pub environments_teardown_failed: i64,
    pub work_items: Vec<(String, i64)>,
    pub stage_durations: Vec<(String, String, i64)>,
    pub adapter_requests: Vec<(String, String, u64)>,
    pub admission_waiting: Vec<(String, i64)>,
    pub sessions_resumed: Vec<(String, i64)>,
    pub budget_exhausted: Vec<(String, i64)>,
    pub usage: Vec<(String, i64, i64)>,
    pub account_utilization: Vec<(String, String, f64)>,
    pub work_packet_bytes: PacketBytes,
    pub webhook_deliveries: Vec<(String, String, String, u64)>,
    pub hook_runs: Vec<(String, String, String, u64)>,
    pub gate_runs: Vec<(String, String, u64)>,
    pub auto_retries: Vec<(String, u64)>,
    pub merge_turns: Vec<(String, u64)>,
    pub merge_dequeues: Vec<(String, u64)>,
    pub merge_requests: Vec<(String, u64)>,
    pub gate_seconds: SecondsHistogram,
    pub advance_seconds: SecondsHistogram,
    pub dod_verifications: Vec<(String, u64)>,
    pub spec_drafts_active: i64,
    pub spec_drafts_finished: Vec<(String, u64)>,
    pub spec_draft_lint_rounds: Vec<(String, u64)>,
    pub config_reloads: Vec<(String, u64)>,
    pub config_checks: Vec<(String, u64)>,
    pub path_scope_violations: Vec<(String, u64)>,
    pub docs_changes: Vec<(String, u64)>,
    pub quick_specs: Vec<(String, String, u64)>,
    pub user_link_authorizations: Vec<(String, String, u64)>,
    pub user_link_refreshes: Vec<(String, String, u64)>,
    pub change_requests_opened: Vec<(String, u64)>,
    pub diagnostic_checks: Vec<(String, String, u64)>,
    pub intake_changes: Vec<(String, u64)>,
    pub chat_actions: Vec<(String, String, u64)>,
    pub review_requests: Vec<(String, u64)>,
    pub review_verdicts: Vec<(String, String, u64)>,
    pub review_format_rejected: Vec<(String, u64)>,
    pub review_posts: Vec<(String, u64)>,
    pub fix_rounds: Vec<(String, u64)>,
    pub fix_exhausted: u64,
    pub regression_checks: Vec<(String, u64)>,
    pub intake_scan_seconds: SecondsHistogram,
}

pub(crate) enum DodVerification {
    Outcome(&'static str),
    Started {
        head_sha: String,
        sources: Vec<&'static str>,
    },
}

pub(crate) fn dod_verification(events: &[Event], after: &Task) -> Option<DodVerification> {
    let target = after
        .stage_run
        .as_ref()
        .filter(|r| r.stage == Stage::Sync)
        .and_then(|r| r.head_sha.clone());
    events.iter().find_map(|e| match e {
        Event::StagePhaseChanged {
            phase: StagePhase::TearingDown,
            ..
        } => Some(DodVerification::Outcome(if target.is_some() {
            "passed"
        } else {
            "proven"
        })),
        Event::StagePhaseChanged {
            phase: StagePhase::Verifying,
            head_sha: Some(head_sha),
            ..
        } => {
            let policy =
                GatePolicy::from_recipe(after.environment.as_ref().and_then(|e| e.recipe.as_ref()));
            let sources = [GateSource::Scm, GateSource::Saka]
                .into_iter()
                .filter(|s| policy.uses(*s))
                .map(GateSource::as_str)
                .collect();
            Some(DodVerification::Started {
                head_sha: head_sha.clone(),
                sources,
            })
        }
        Event::WorkItemBlocked {
            code: BlockerCode::DodFailed,
            params,
            ..
        } => {
            let reason = params.get("reason").and_then(serde_json::Value::as_str);
            matches!(
                reason,
                Some("verify_failed" | "no_checks" | "required_missing")
            )
            .then_some(DodVerification::Outcome("failed"))
        }
        _ => None,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Readiness {
    pub db: bool,
    pub migrations: bool,
    pub worker: bool,
    pub reconciler: bool,
    pub sessions_reattached: bool,
}

impl Readiness {
    pub fn checks(&self) -> [(&'static str, bool); 4] {
        [
            ("db", self.db),
            ("migrations", self.migrations),
            ("worker", self.worker),
            ("reconciler", self.reconciler && self.sessions_reattached),
        ]
    }

    pub fn failing(&self) -> Vec<&'static str> {
        self.checks()
            .into_iter()
            .filter(|(_, ok)| !ok)
            .map(|(name, _)| name)
            .collect()
    }
}

impl Engine {
    pub fn metrics(&self) -> Result<MetricsSnapshot> {
        let now = self.now();
        let jobs = self.store.job_counts()?;
        let oldest = self.store.oldest_ready_job_age_ms(now)?;
        let (sessions, pending, leftover, items, durations) = self.store.read(|c| {
            Ok((
                list::live_session_counts(c)?,
                list::pending_permission_count(c)?,
                list::teardown_failed_environment_count(c)?,
                list::work_item_counts(c)?,
                list::stage_durations(c)?,
            ))
        })?;
        let drafts_active = self.store.read(saka_store::draft::active_count)?;
        let (resumed, exhausted, usage, quotas) = self.store.read(|c| {
            Ok((
                list::resumed_session_counts(c)?,
                list::budget_exhausted_counts(c)?,
                list::usage_totals(c)?,
                saka_store::read::account_quotas(c)?,
            ))
        })?;
        Ok(MetricsSnapshot {
            sessions_resumed: resumed,
            budget_exhausted: exhausted,
            usage,
            account_utilization: quotas
                .into_iter()
                .map(|q| (q.account, q.window, q.utilization))
                .collect(),
            jobs,
            oldest_ready_job_age_ms: oldest,
            job_runs: copy(&self.counters.job_runs),
            sessions_active: sessions,
            permission_requests_pending: pending,
            environments_teardown_failed: leftover,
            work_items: items,
            stage_durations: durations,
            adapter_requests: copy(&self.counters.adapter_requests),
            admission_waiting: self.counters.waiting_counts(),
            work_packet_bytes: self.counters.packet_bytes(),
            webhook_deliveries: self.counters.webhook_counts(),
            hook_runs: self.counters.hook_run_counts(),
            gate_runs: copy(&self.counters.gate_runs),
            auto_retries: copy(&self.counters.auto_retries)
                .into_iter()
                .map(|(code, _, n)| (code, n))
                .collect(),
            merge_turns: copy(&self.counters.merge_turns)
                .into_iter()
                .map(|(outcome, _, n)| (outcome, n))
                .collect(),
            merge_dequeues: copy(&self.counters.merge_dequeues)
                .into_iter()
                .map(|(reason, _, n)| (reason, n))
                .collect(),
            merge_requests: copy(&self.counters.merge_requests)
                .into_iter()
                .map(|(by, _, n)| (by, n))
                .collect(),
            gate_seconds: snapshot_of(&self.counters.gate_seconds),
            advance_seconds: snapshot_of(&self.counters.advance_seconds),
            dod_verifications: copy(&self.counters.dod)
                .into_iter()
                .map(|(outcome, _, n)| (outcome, n))
                .collect(),
            spec_drafts_active: drafts_active,
            spec_drafts_finished: copy(&self.counters.drafts_finished)
                .into_iter()
                .map(|(state, _, n)| (state, n))
                .collect(),
            spec_draft_lint_rounds: copy(&self.counters.draft_lint_rounds)
                .into_iter()
                .map(|(outcome, _, n)| (outcome, n))
                .collect(),
            diagnostic_checks: copy(&self.counters.diagnostic_checks),
            config_reloads: copy(&self.counters.config_reloads)
                .into_iter()
                .map(|(outcome, _, n)| (outcome, n))
                .collect(),
            config_checks: copy(&self.counters.config_checks)
                .into_iter()
                .map(|(outcome, _, n)| (outcome, n))
                .collect(),
            path_scope_violations: copy(&self.counters.path_scope_violations)
                .into_iter()
                .map(|(stage, _, n)| (stage, n))
                .collect(),
            docs_changes: ["merged", "skipped"]
                .iter()
                .map(|outcome| {
                    let n = copy(&self.counters.docs_changes)
                        .into_iter()
                        .find(|(o, _, _)| o == outcome)
                        .map_or(0, |(_, _, n)| n);
                    ((*outcome).to_string(), n)
                })
                .collect(),
            quick_specs: copy(&self.counters.quick_specs),
            user_link_authorizations: copy(&self.counters.user_link_authorizations),
            user_link_refreshes: copy(&self.counters.user_link_refreshes),
            change_requests_opened: copy(&self.counters.change_requests_opened)
                .into_iter()
                .map(|(credential, _, n)| (credential, n))
                .collect(),
            chat_actions: copy(&self.counters.chat_actions),
            intake_changes: copy(&self.counters.intake_changes)
                .into_iter()
                .map(|(outcome, _, n)| (outcome, n))
                .collect(),
            review_verdicts: copy(&self.counters.review_verdicts),
            review_format_rejected: copy(&self.counters.review_format_rejected)
                .into_iter()
                .map(|(code, _, n)| (code, n))
                .collect(),
            review_posts: copy(&self.counters.review_posts)
                .into_iter()
                .map(|(event, _, n)| (event, n))
                .collect(),
            fix_rounds: copy(&self.counters.fix_rounds)
                .into_iter()
                .map(|(cause, _, n)| (cause, n))
                .collect(),
            regression_checks: copy(&self.counters.regression_checks)
                .into_iter()
                .map(|(outcome, _, n)| (outcome, n))
                .collect(),
            fix_exhausted: copy(&self.counters.fix_exhausted)
                .into_iter()
                .map(|(_, _, n)| n)
                .sum(),
            review_requests: copy(&self.counters.review_requests)
                .into_iter()
                .map(|(cause, _, n)| (cause, n))
                .collect(),
            intake_scan_seconds: snapshot_of(&self.counters.intake_scan_seconds),
        })
    }

    pub fn readiness(&self) -> Readiness {
        let db = self
            .store
            .read(|c| Ok(c.query_row("SELECT 1", [], |r| r.get::<_, i64>(0))?))
            .is_ok();
        let migrations = self.store.schema_version().ok().flatten().is_some();
        Readiness {
            db,
            migrations,
            worker: self.counters.worker_started.load(Ordering::SeqCst),
            reconciler: self.counters.reconciler_started.load(Ordering::SeqCst),
            sessions_reattached: self.store.job_counts().is_ok_and(|counts| {
                !counts.iter().any(|(kind, state, n)| {
                    kind == "session.reattach"
                        && matches!(state.as_str(), "queued" | "leased" | "failed")
                        && *n > 0
                })
            }),
        }
    }
}
