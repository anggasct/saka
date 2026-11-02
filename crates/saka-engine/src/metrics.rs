use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use saka_core::admission::Waiting;
use saka_core::ids::WorkItemId;
use saka_core::job::JobKind;
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
    gate_runs: Mutex<BTreeMap<(String, String), u64>>,
    gate_seconds: Mutex<SecondsHistogram>,
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

#[derive(Debug, Clone, PartialEq, Default)]
pub struct SecondsHistogram {
    pub buckets: Vec<(f64, u64)>,
    pub count: u64,
    pub sum: f64,
}

pub const PACKET_BYTE_BUCKETS: [u64; 6] = [4_096, 16_384, 65_536, 131_072, 262_144, 524_288];

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PacketBytes {
    pub buckets: Vec<(u64, u64)>,
    pub count: u64,
    pub sum: u64,
}

impl Counters {
    pub(crate) fn gate_run(&self, source: &str, outcome: &str, seconds: f64) {
        bump(&self.gate_runs, source, outcome);
        if let Ok(mut h) = self.gate_seconds.lock() {
            if h.buckets.is_empty() {
                h.buckets = GATE_SECONDS_BUCKETS.iter().map(|b| (*b, 0)).collect();
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
    pub gate_runs: Vec<(String, String, u64)>,
    pub gate_seconds: SecondsHistogram,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Readiness {
    pub db: bool,
    pub migrations: bool,
    pub worker: bool,
    pub reconciler: bool,
    pub sessions_reattached: bool,
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
            gate_runs: copy(&self.counters.gate_runs),
            gate_seconds: self
                .counters
                .gate_seconds
                .lock()
                .map(|h| h.clone())
                .unwrap_or_default(),
        })
    }

    pub fn readiness(&self) -> Readiness {
        let db = self
            .store
            .read(|c| Ok(c.query_row("SELECT 1", [], |r| r.get::<_, i64>(0))?))
            .is_ok();
        // The store migrates when it opens, so an engine always has a migrated store.
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
