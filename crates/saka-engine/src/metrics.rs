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

impl Counters {
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Readiness {
    pub db: bool,
    pub migrations: bool,
    pub worker: bool,
    pub reconciler: bool,
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
        Ok(MetricsSnapshot {
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
        }
    }
}
