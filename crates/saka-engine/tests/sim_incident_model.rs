#![allow(clippy::unwrap_used, reason = "tests fail loudly on a broken setup")]

use std::collections::BTreeSet;

use saka_core::Ctx;
use saka_core::environment::{self, RawEnvironment};
use saka_core::ids::IdSource;
use saka_core::incident::{self, Arrival, IncidentSignal, Observed, RawSource};
use saka_core::time::Timestamp;
use saka_core::vocab::{CheckId, DiagnosticStatus, IncidentSeverity, IncidentState, SignalStatus};
use saka_engine::{DiagnosticFilter, IncidentQuery, Page, Suspect};
use saka_store::Batch;
use saka_testkit::{Sim, SimConfig};
use uuid::Uuid;

const STEPS: usize = 30_000;

struct Ids(u128);

impl IdSource for Ids {
    fn next_id(&mut self) -> Uuid {
        self.0 += 1;
        Uuid::from_u128(self.0)
    }
}

fn source(name: &str, extra: impl FnOnce(&mut RawSource)) -> saka_core::incident::Source {
    let mut raw = RawSource {
        name: name.into(),
        project: "tasks".into(),
        kind: "alertmanager".into(),
        secret_file: "/nonexistent/incident.secret".into(),
        ..RawSource::default()
    };
    extra(&mut raw);
    saka_core::incident::Source::validate(&raw).unwrap()
}

fn staging() -> RawEnvironment {
    RawEnvironment {
        project: "tasks".into(),
        environment: "staging".into(),
        action: "run_host".into(),
        command: Some("./deploy.sh staging".into()),
        on_failure: Some("revert".into()),
        ..RawEnvironment::default()
    }
}

fn open_incident(
    sim: &Sim,
    source: &str,
    fingerprint: &str,
    severity: IncidentSeverity,
    deployment: Option<saka_core::ids::DeploymentId>,
    at: i64,
) -> saka_core::incident::Incident {
    let signal = IncidentSignal {
        fingerprint: fingerprint.into(),
        status: SignalStatus::Firing,
        severity,
        title: format!("{fingerprint} is down"),
        summary: String::new(),
        environment: Some("staging".into()),
        url: None,
        started_at: None,
        labels: Default::default(),
    };
    let arrival = Arrival {
        project_id: sim.project_id,
        source,
        max_open: 50,
        live_in_source: 0,
        deployment_id: deployment,
    };
    let mut ids = Ids(u128::from(at.unsigned_abs()) * 1_000_000);
    let mut ctx = Ctx {
        now: Timestamp(at),
        ids: &mut ids,
    };
    let Observed::Opened(events) = incident::observe(None, &signal, &arrival, &mut ctx) else {
        panic!("expected an incident");
    };
    let incident = incident::evolve_all(None, &events, Timestamp(at)).unwrap();
    let mut batch = Batch::new(Timestamp(at));
    batch.incidents.push(incident.clone());
    sim.engine().store().commit(&batch).unwrap();
    incident
}

fn page() -> Page {
    Page {
        after: None,
        limit: 50,
    }
}

#[test]
fn a_project_lists_only_its_own_sources_with_their_endpoint_and_open_count() {
    let sim = Sim::new(SimConfig {
        incident_sources: vec![
            source("prod-alerts", |_| {}),
            source("errors", |s| {
                s.kind = "sentry".into();
                s.action = Some("draft".into());
                s.max_open = Some(5);
            }),
            source("elsewhere", |s| s.project = "other".into()),
        ],
        public_url: Some("https://saka.example.com/".into()),
        ..SimConfig::default()
    });
    open_incident(
        &sim,
        "prod-alerts",
        "a",
        IncidentSeverity::High,
        None,
        1_000,
    );
    open_incident(&sim, "prod-alerts", "b", IncidentSeverity::Low, None, 2_000);

    let sources = sim.engine().incident_sources(sim.project_id).unwrap();
    let summary: Vec<_> = sources
        .iter()
        .map(|v| (v.source.name.as_str(), v.open, v.endpoint.as_deref()))
        .collect();
    assert_eq!(
        summary,
        [
            (
                "errors",
                0,
                Some("https://saka.example.com/hooks/incident/errors")
            ),
            (
                "prod-alerts",
                2,
                Some("https://saka.example.com/hooks/incident/prod-alerts")
            ),
        ]
    );
    assert_eq!(sources[1].endpoint_path, "/hooks/incident/prod-alerts");
    assert_eq!(sources[0].source.max_open, 5);
}

#[test]
fn without_a_public_url_a_source_has_only_its_path() {
    let sim = Sim::new(SimConfig {
        incident_sources: vec![source("prod-alerts", |_| {})],
        ..SimConfig::default()
    });
    let sources = sim.engine().incident_sources(sim.project_id).unwrap();
    assert_eq!(sources[0].endpoint, None);
}

#[test]
fn incidents_list_most_recently_seen_first_and_filter() {
    let sim = Sim::new(SimConfig::default());
    let old = open_incident(
        &sim,
        "prod-alerts",
        "old",
        IncidentSeverity::Low,
        None,
        1_000,
    );
    let new = open_incident(
        &sim,
        "prod-alerts",
        "new",
        IncidentSeverity::Critical,
        None,
        3_000,
    );
    let other = open_incident(&sim, "errors", "mid", IncidentSeverity::High, None, 2_000);

    let all = sim
        .engine()
        .incidents(sim.project_id, IncidentQuery::default(), &page())
        .unwrap();
    let ids: Vec<_> = all.items.iter().map(|r| r.incident.id).collect();
    assert_eq!(ids, [new.id, other.id, old.id]);

    let critical = sim
        .engine()
        .incidents(
            sim.project_id,
            IncidentQuery {
                severity: Some(IncidentSeverity::Critical),
                ..IncidentQuery::default()
            },
            &page(),
        )
        .unwrap();
    assert_eq!(critical.items.len(), 1);
    assert_eq!(critical.items[0].incident.id, new.id);

    let handled = sim
        .engine()
        .incidents(
            sim.project_id,
            IncidentQuery {
                state: Some(IncidentState::Handling),
                ..IncidentQuery::default()
            },
            &page(),
        )
        .unwrap();
    assert!(handled.items.is_empty());

    let first_page = sim
        .engine()
        .incidents(
            sim.project_id,
            IncidentQuery::default(),
            &Page {
                after: None,
                limit: 2,
            },
        )
        .unwrap();
    assert_eq!(first_page.items.len(), 2);
    let next = first_page.next.unwrap();
    let rest = sim
        .engine()
        .incidents(
            sim.project_id,
            IncidentQuery::default(),
            &Page {
                after: Some(next),
                limit: 2,
            },
        )
        .unwrap();
    assert_eq!(rest.items.len(), 1);
    assert_eq!(rest.items[0].incident.id, old.id);
    assert!(rest.next.is_none());
}

#[test]
fn an_unknown_incident_or_project_is_not_found() {
    let sim = Sim::new(SimConfig::default());
    let missing = saka_core::ids::IncidentId(Uuid::from_u128(1));
    assert!(sim.engine().incident(missing).is_err());
    let unknown = saka_core::ids::ProjectId(Uuid::from_u128(2));
    assert!(sim.engine().incident_sources(unknown).is_err());
    assert!(
        sim.engine()
            .incidents(unknown, IncidentQuery::default(), &page())
            .is_err()
    );
}

#[test]
fn an_incident_shows_the_deployment_it_is_linked_to_with_its_range_and_suspects() {
    let mut sim = Sim::new(SimConfig {
        environments: environment::parse_all(&[staging()]).unwrap(),
        ..SimConfig::default()
    });
    sim.approve();
    assert!(sim.run_to_shipped(STEPS), "{:?}", sim.outcome());
    let mut deployments = sim
        .engine()
        .deployments(sim.project_id, None, None, &page())
        .unwrap()
        .items;
    deployments.sort_by_key(|r| (r.deployment.queued_at, r.deployment.id.0));
    let last = &deployments.last().unwrap().deployment;
    let created = open_incident(
        &sim,
        "prod-alerts",
        "fp",
        IncidentSeverity::High,
        Some(last.id),
        9_000_000,
    );

    let detail = sim.engine().incident(created.id).unwrap();
    assert_eq!(detail.row.deployment.as_ref().unwrap().id, last.id);
    assert_eq!(
        detail.range,
        Some((last.previous_sha.clone(), last.sha.clone().unwrap()))
    );
    assert_eq!(detail.suspects.len(), 1);
    let Suspect::Task { key, change, .. } = &detail.suspects[0] else {
        panic!("expected a task suspect, got {:?}", detail.suspects[0]);
    };
    assert!(!key.is_empty());
    assert!(change.is_some(), "the task's merged change is named");

    let listed = sim
        .engine()
        .incidents(sim.project_id, IncidentQuery::default(), &page())
        .unwrap();
    assert_eq!(listed.items[0].deployment.as_ref().unwrap().id, last.id);

    let unlinked = open_incident(
        &sim,
        "prod-alerts",
        "none",
        IncidentSeverity::Low,
        None,
        9_100_000,
    );
    let bare = sim.engine().incident(unlinked.id).unwrap();
    assert!(bare.row.deployment.is_none() && bare.range.is_none() && bare.suspects.is_empty());
}

fn secret(dir: &std::path::Path, name: &str, content: &str, mode: u32) -> String {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    std::fs::write(&path, content).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
    path.to_string_lossy().into_owned()
}

fn incident_findings(sim: &Sim) -> Vec<(DiagnosticStatus, String, String)> {
    let report = sim
        .block_on(sim.engine().diagnostics(
            &DiagnosticFilter {
                checks: Some(BTreeSet::from([CheckId::IncidentSource])),
                project_id: None,
            },
            "test",
        ))
        .unwrap();
    report
        .checks
        .into_iter()
        .map(|c| {
            (
                c.finding.status,
                c.finding.code,
                c.finding.params["source"]
                    .as_str()
                    .unwrap_or("")
                    .to_string(),
            )
        })
        .collect()
}

#[test]
fn doctor_has_nothing_to_say_without_sources() {
    let sim = Sim::new(SimConfig::default());
    assert_eq!(
        incident_findings(&sim),
        [(
            DiagnosticStatus::Skip,
            "incident_not_configured".into(),
            String::new()
        )]
    );
}

#[test]
fn doctor_reports_an_unregistered_project_a_wrong_mode_a_missing_and_an_empty_secret_file() {
    let dir = tempfile::tempdir().unwrap();
    let good = secret(dir.path(), "good", "s3cr3t\n", 0o600);
    let open = secret(dir.path(), "open", "s3cr3t", 0o644);
    let empty = secret(dir.path(), "empty", "\n", 0o600);
    let missing = dir.path().join("absent").to_string_lossy().into_owned();
    let with_file = |name: &str, file: &str, project: &str| {
        source(name, |s| {
            s.secret_file = file.into();
            s.project = project.into();
        })
    };
    let sim = Sim::new(SimConfig {
        incident_sources: vec![
            with_file("fine", &good, "tasks"),
            with_file("ghost", &good, "no-such-project"),
            with_file("open", &open, "tasks"),
            with_file("blank", &empty, "tasks"),
            with_file("absent", &missing, "tasks"),
        ],
        ..SimConfig::default()
    });
    let found: Vec<(DiagnosticStatus, String, String)> = incident_findings(&sim);
    let codes: Vec<(&str, &str)> = found
        .iter()
        .map(|(_, code, source)| (code.as_str(), source.as_str()))
        .collect();
    assert_eq!(
        codes,
        [
            ("incident_project_unknown", "ghost"),
            ("incident_secret_mode", "open"),
            ("incident_secret_empty", "blank"),
            ("incident_secret_unreadable", "absent"),
        ]
    );
    assert!(
        found
            .iter()
            .all(|(status, _, _)| *status == DiagnosticStatus::Fail)
    );
}

#[test]
fn doctor_passes_when_every_source_is_sound() {
    let dir = tempfile::tempdir().unwrap();
    let file = secret(dir.path(), "good", "s3cr3t\n", 0o600);
    let sim = Sim::new(SimConfig {
        incident_sources: vec![source("prod-alerts", |s| s.secret_file = file.clone())],
        ..SimConfig::default()
    });
    let found = incident_findings(&sim);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].0, DiagnosticStatus::Pass);
    assert_eq!(found[0].1, "incident_source_ok");
}
