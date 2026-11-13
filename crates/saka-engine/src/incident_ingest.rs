use std::collections::BTreeMap;

use saka_core::Ctx;
use saka_core::incident::{self, Arrival, Incident, IncidentSignal, Observed, Source};
use saka_core::job::{Job, JobKind};
use saka_core::model::Project;
use saka_core::redact::Redactor;
use saka_core::rejection::Rejection;
use saka_core::vocab::{IncidentState, SignalStatus};
use saka_ports::{IdsSource, RawDelivery};
use saka_store::incident::DeliveryRow;
use saka_store::{Batch, deployment, incident as rows, list, read};
use serde_json::json;

use crate::{Engine, EngineError, Result};

pub const RATE_LIMIT_PER_MINUTE: u32 = 120;
const WINDOW_MS: i64 = 60_000;
pub const INTAKE_ACTOR: &str = "system:incident";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReceiptOutcome {
    Accepted,
    Duplicate,
    Ignored,
}

impl ReceiptOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            ReceiptOutcome::Accepted => "accepted",
            ReceiptOutcome::Duplicate => "duplicate",
            ReceiptOutcome::Ignored => "ignored",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncidentReceipt {
    pub delivery_id: String,
    pub outcome: ReceiptOutcome,
    pub signals: u32,
    pub opened: u32,
    pub updated: u32,
    pub limited: u32,
    pub truncated: u32,
}

fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

struct Tally {
    opened: u32,
    updated: u32,
    resolved: u32,
    ignored: u32,
    limited: u32,
}

impl Engine {
    pub fn incident_source_config(&self, name: &str) -> Option<&Source> {
        self.cfg.incident_sources.iter().find(|s| s.name == name)
    }

    pub fn incident_rate_limit(&self, source: &str) -> std::result::Result<(), u64> {
        let now = self.now().0;
        let window = now.div_euclid(WINDOW_MS);
        let Ok(mut limits) = self.incident_limits.lock() else {
            return Ok(());
        };
        let entry = limits.entry(source.to_string()).or_insert((window, 0));
        if entry.0 != window {
            *entry = (window, 0);
        }
        if entry.1 >= RATE_LIMIT_PER_MINUTE {
            let left = (window + 1) * WINDOW_MS - now;
            return Err(u64::try_from((left + 999) / 1000).unwrap_or(1).max(1));
        }
        entry.1 += 1;
        Ok(())
    }

    pub fn incident_rejected(&self, source: &str, code: &str) {
        self.counters.incident_delivery(source, "rejected");
        tracing::warn!(source, code, "intake.incident.rejected");
    }

    fn project_of_source(&self, source: &Source) -> Result<Project> {
        let found = self.store.read(|c| {
            for id in list::project_ids(c)? {
                if let Some(project) = read::load_project(c, id)?
                    && source.is_for(project.id, &project.name)
                {
                    return Ok(Some(project));
                }
            }
            Ok(None)
        })?;
        found.ok_or(EngineError::Rejected(Rejection::NotFound))
    }

    pub fn receive_incident(
        &self,
        source_name: &str,
        headers: &[(String, String)],
        body: &[u8],
    ) -> Result<IncidentReceipt> {
        let source = self
            .incident_source_config(source_name)
            .ok_or(EngineError::Rejected(Rejection::NotFound))?;
        let decoded = self
            .ports
            .signals
            .decode(
                source_name,
                &RawDelivery {
                    headers,
                    body,
                    now: self.now(),
                },
            )
            .inspect_err(|e| self.incident_rejected(source_name, &e.code))?;
        let _guard = self.locked()?;
        let now = self.now();
        let seen = self
            .store
            .read(|c| rows::delivery_seen(c, source_name, &decoded.delivery_key))?;
        let fresh = |window: Option<u64>, at: saka_core::time::Timestamp| {
            window
                .is_some_and(|secs| now.since(at) >= i64::try_from(secs).unwrap_or(i64::MAX) * 1000)
        };
        let receipt = |outcome, signals, tally: Option<&Tally>, truncated| IncidentReceipt {
            delivery_id: decoded.delivery_key.clone(),
            outcome,
            signals,
            opened: tally.map_or(0, |t| t.opened),
            updated: tally.map_or(0, |t| t.updated + t.resolved),
            limited: tally.map_or(0, |t| t.limited),
            truncated,
        };
        if let Some(at) = seen
            && !fresh(decoded.dedup_window_secs, at)
        {
            self.counters.incident_delivery(source_name, "duplicate");
            return Ok(receipt(ReceiptOutcome::Duplicate, 0, None, 0));
        }
        let project = self.project_of_source(source)?;
        let truncated = count(decoded.truncated);
        let signals = count(decoded.signals.len());
        let mut batch = Batch::new(now);
        let tally = if decoded.ignored {
            Tally {
                opened: 0,
                updated: 0,
                resolved: 0,
                ignored: 0,
                limited: 0,
            }
        } else {
            self.apply_signals(&mut batch, source, &project, &decoded.signals, now)?
        };
        let outcome = if decoded.ignored {
            ReceiptOutcome::Ignored
        } else {
            ReceiptOutcome::Accepted
        };
        batch.incident_deliveries.push(DeliveryRow {
            id: self.new_id(),
            source: source_name.to_string(),
            delivery_key: decoded.delivery_key.clone(),
            received_at: now,
            outcome: if decoded.ignored {
                "ignored"
            } else {
                "accepted"
            },
            signals,
        });
        self.commit(&batch)?;
        self.counters
            .incident_delivery(source_name, outcome.as_str());
        for (label, n) in [
            ("opened", tally.opened),
            ("updated", tally.updated),
            ("resolved", tally.resolved),
            ("ignored", tally.ignored),
            ("limited", tally.limited),
            ("truncated", truncated),
        ] {
            for _ in 0..n {
                self.counters.incident_signal(source_name, label);
            }
        }
        tracing::info!(
            source = source_name,
            outcome = outcome.as_str(),
            signals,
            opened = tally.opened,
            updated = tally.updated + tally.resolved,
            limited = tally.limited,
            truncated,
            "intake.incident.received"
        );
        Ok(receipt(outcome, signals, Some(&tally), truncated))
    }

    fn apply_signals(
        &self,
        batch: &mut Batch,
        source: &Source,
        project: &Project,
        signals: &[IncidentSignal],
        now: saka_core::time::Timestamp,
    ) -> Result<Tally> {
        let known: Vec<String> = self
            .environments_of(project)
            .iter()
            .map(|e| e.name.clone())
            .collect();
        let redactor = Redactor::new(&[]);
        let cause = self.cause(INTAKE_ACTOR);
        let mut live = i64::from(self.store.read(|c| rows::live_count(c, &source.name))?);
        let mut working: BTreeMap<String, Incident> = BTreeMap::new();
        let mut tally = Tally {
            opened: 0,
            updated: 0,
            resolved: 0,
            ignored: 0,
            limited: 0,
        };
        for raw in signals {
            let signal = self.clean_signal(raw, source, &known, &redactor);
            let latest = match working.get(&signal.fingerprint) {
                Some(found) => Some(found.clone()),
                None => self
                    .store
                    .read(|c| rows::latest_of(c, &source.name, &signal.fingerprint))?,
            };
            let deployment_id = match (&signal.environment, signal.status) {
                (Some(environment), SignalStatus::Firing) => {
                    let at = signal.started_at.unwrap_or(now);
                    self.store
                        .read(|c| {
                            deployment::last_succeeded_before(c, project.id, environment, at)
                        })?
                        .map(|d| d.id)
                }
                _ => None,
            };
            let arrival = Arrival {
                project_id: project.id,
                source: &source.name,
                max_open: source.max_open,
                live_in_source: u32::try_from(live.max(0)).unwrap_or(u32::MAX),
                deployment_id,
            };
            let mut ids = IdsSource(&*self.ports.ids);
            let mut ctx = Ctx { now, ids: &mut ids };
            let (events, opened) =
                match incident::observe(latest.as_ref(), &signal, &arrival, &mut ctx) {
                    Observed::Opened(events) => (events, true),
                    Observed::Updated(events) => (events, false),
                    Observed::Ignored => {
                        tally.ignored += 1;
                        continue;
                    }
                    Observed::Limited => {
                        tally.limited += 1;
                        continue;
                    }
                };
            let before_live = latest.as_ref().is_some_and(|i| i.state.is_live());
            let base = if opened { None } else { latest };
            let after = incident::evolve_all(base, &events, now).ok_or_else(|| {
                EngineError::Internal("incident dropped by its signal events".into())
            })?;
            for event in &events {
                self.push_event(batch, event, Some(project.id), &cause, now);
            }
            if opened {
                tally.opened += 1;
                live += 1;
            } else if before_live && !after.state.is_live() {
                tally.resolved += 1;
                live -= 1;
            } else {
                tally.updated += 1;
            }
            if after.state == IncidentState::Open
                && after.draft_id.is_none()
                && source.drafts_automatically(after.severity)
            {
                let job = Job::new(
                    JobKind::IncidentDraft,
                    after.id,
                    json!({ "incident_id": after.id }),
                    now,
                );
                self.push_job(batch, job, Some(project.id), &cause);
            }
            tracing::info!(
                incident_id = %after.id,
                source = %source.name,
                state = after.state.as_str(),
                severity = after.severity.as_str(),
                occurrences = after.occurrences,
                "intake.incident.signal"
            );
            batch.incidents.retain(|i| i.id != after.id);
            batch.incidents.push(after.clone());
            working.insert(after.fingerprint.clone(), after);
        }
        Ok(tally)
    }

    fn clean_signal(
        &self,
        raw: &IncidentSignal,
        source: &Source,
        known: &[String],
        redactor: &Redactor,
    ) -> IncidentSignal {
        let mut signal = raw.clone();
        signal.environment = raw
            .environment
            .as_deref()
            .and_then(|e| source.environment_of(e))
            .filter(|name| known.contains(name));
        signal.title = redactor.text(&raw.title).into_owned();
        signal.summary = redactor.text(&raw.summary).into_owned();
        signal.labels = raw
            .labels
            .iter()
            .map(|(k, v)| (k.clone(), redactor.text(v).into_owned()))
            .collect();
        signal
    }
}
