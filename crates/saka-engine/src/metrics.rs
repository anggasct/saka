use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use saka_core::job::JobKind;
use saka_store::list;

use crate::worker::JobResult;
use crate::{Engine, Result};

#[derive(Debug, Default)]
pub struct Counters {
    job_runs: Mutex<BTreeMap<(String, String), u64>>,
    adapter_requests: Mutex<BTreeMap<(String, String), u64>>,
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

#[derive(Debug, Clone, PartialEq)]
pub struct MetricsSnapshot {
    pub jobs: Vec<(String, String, i64)>,
    pub oldest_ready_job_age_ms: Option<i64>,
    pub job_runs: Vec<(String, String, u64)>,
    pub sessions_active: Vec<(String, i64)>,
    pub permission_requests_pending: i64,
    pub work_items: Vec<(String, i64)>,
    pub stage_durations: Vec<(String, String, i64)>,
    pub adapter_requests: Vec<(String, String, u64)>,
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
        let (sessions, pending, items, durations) = self.store.read(|c| {
            Ok((
                list::live_session_counts(c)?,
                list::pending_permission_count(c)?,
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
            work_items: items,
            stage_durations: durations,
            adapter_requests: copy(&self.counters.adapter_requests),
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
