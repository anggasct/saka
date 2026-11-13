#![allow(clippy::unwrap_used, reason = "tests fail loudly on a broken setup")]

use std::collections::BTreeSet;

use saka_core::environment::{self, RawEnvironment};
use saka_core::secret::SecretText;
use saka_core::vocab::{CheckId, DiagnosticStatus};
use saka_engine::{Cause, DiagnosticFilter};
use saka_testkit::{Layout, SHOP_REPO, Sim, SimConfig, repo_entry, saka_toml};

fn raw(name: &str) -> RawEnvironment {
    RawEnvironment {
        project: "shop".into(),
        environment: name.into(),
        action: "run_host".into(),
        command: Some("./deploy.sh".into()),
        on_failure: Some("revert".into()),
        ..RawEnvironment::default()
    }
}

fn webhook(name: &str) -> RawEnvironment {
    RawEnvironment {
        action: "webhook".into(),
        command: None,
        url: Some("https://ci.example.com/hooks/deploy".into()),
        secret: Some("DEPLOY_SIGNING_KEY".into()),
        ..raw(name)
    }
}

fn sim(environments: &[RawEnvironment], verify: &str) -> Sim {
    let mut text = saka_toml("shop", &[repo_entry("shop", SHOP_REPO, "")]);
    text.push_str(verify);
    let layout = Layout {
        saka_toml: text,
        ..Layout::default()
    };
    Sim::multi_repo(
        SimConfig {
            environments: environment::parse_all(environments).unwrap(),
            ..SimConfig::default()
        },
        &layout,
    )
}

const VERIFY: &str = r#"
[verify.staging]
ready = "curl -fsS \"$SAKA_DEPLOY_URL/version\" | grep -qx \"$SAKA_SHA\""
ready_timeout_secs = 120
commands = ["npm run smoke", "npm run e2e"]

[verify.qa]
commands = ["true"]
"#;

fn findings(sim: &Sim, check: CheckId) -> Vec<(DiagnosticStatus, String)> {
    let report = sim
        .block_on(sim.engine().diagnostics(
            &DiagnosticFilter {
                checks: Some(BTreeSet::from([check])),
                project_id: Some(sim.project_id),
            },
            "test",
        ))
        .unwrap();
    report
        .checks
        .into_iter()
        .map(|c| (c.finding.status, c.finding.code))
        .collect()
}

#[test]
fn the_verification_of_saka_toml_is_stored_with_the_project() {
    let sim = sim(&[raw("staging")], VERIFY);
    let verify = &sim.project().project.verify;
    assert_eq!(verify.keys().collect::<Vec<_>>(), ["qa", "staging"]);
    let staging = &verify["staging"];
    assert!(
        staging
            .ready
            .as_deref()
            .unwrap()
            .contains("SAKA_DEPLOY_URL")
    );
    assert_eq!(staging.ready_timeout_secs, 120);
    assert_eq!(staging.ready_interval_secs, 15);
    assert_eq!(staging.commands, ["npm run smoke", "npm run e2e"]);
    assert_eq!(staging.timeout_secs, 300);
}

#[test]
fn environments_show_their_binding_verification_and_warnings_but_never_the_action() {
    let ghost = RawEnvironment {
        repo: Some("api".into()),
        ..raw("ghost")
    };
    let sim = sim(&[raw("staging"), webhook("preview"), ghost], VERIFY);
    let views = sim.engine().environments(sim.project_id).unwrap();
    let names: Vec<&str> = views.iter().map(|v| v.name.as_str()).collect();
    assert_eq!(
        names,
        ["preview", "staging"],
        "an environment bound to no repo is left out"
    );
    let staging = &views[1];
    assert_eq!(staging.repo.name, "shop");
    assert_eq!(staging.branch, "main");
    assert!(staging.gate);
    assert_eq!(
        (staging.verification.ready, staging.verification.commands),
        (true, 2)
    );
    assert!(staging.verification.warnings.is_empty());
    let preview = &views[0];
    assert_eq!(
        preview.verification.warnings,
        ["verify_missing", "ready_missing"]
    );
    assert!(
        preview.queued.is_none() && preview.active.is_none() && preview.last_succeeded.is_none()
    );
}

#[test]
fn doctor_reports_an_unbound_environment_a_missing_secret_and_an_unknown_verification() {
    let ghost = RawEnvironment {
        repo: Some("api".into()),
        ..raw("ghost")
    };
    let sim = sim(&[raw("staging"), webhook("preview"), ghost], VERIFY);
    let config = findings(&sim, CheckId::DeployConfig);
    let codes: Vec<&str> = config.iter().map(|(_, c)| c.as_str()).collect();
    assert_eq!(
        codes,
        [
            "deploy_secret_missing",
            "deploy_repo_unknown",
            "verify_environment_unknown"
        ]
    );
    let missing = findings(&sim, CheckId::DeployVerifyMissing);
    assert_eq!(
        missing,
        [
            (DiagnosticStatus::Warn, "deploy_verify_missing".into()),
            (DiagnosticStatus::Warn, "deploy_verify_missing".into()),
        ]
    );
    assert_eq!(
        findings(&sim, CheckId::DeployReadyMissing),
        [(DiagnosticStatus::Warn, "deploy_ready_missing".into())]
    );
    sim.engine()
        .set_secret(
            sim.project_id,
            "DEPLOY_SIGNING_KEY",
            &SecretText::new("signing-key-abcdef-123456".into()),
            &Cause {
                actor: "operator".into(),
                causation_id: None,
                correlation_id: uuid::Uuid::from_u128(42),
            },
            None,
        )
        .unwrap();
    let after = findings(&sim, CheckId::DeployConfig);
    assert!(
        after.iter().all(|(_, c)| c != "deploy_secret_missing"),
        "{after:?}"
    );
}

#[test]
fn a_project_without_environments_has_nothing_to_diagnose() {
    let sim = sim(&[], "");
    assert_eq!(
        findings(&sim, CheckId::DeployConfig),
        [(DiagnosticStatus::Skip, "deploy_not_configured".into())]
    );
    assert!(
        sim.engine()
            .environments(sim.project_id)
            .unwrap()
            .is_empty()
    );
}

mod stored {
    use saka_core::Ctx;
    use saka_core::deploy::{self, CheckRecord, Input};
    use saka_core::ids::{IdSource, RepoId};
    use saka_core::model::Coded;
    use saka_core::time::Timestamp;
    use saka_core::vocab::{
        DeployActionKind, DeployCause, DeployCheckState, DeployPhase, DeploymentState,
    };
    use saka_engine::Page;
    use saka_store::Batch;
    use uuid::Uuid;

    use super::*;

    struct Ids(u128);

    impl IdSource for Ids {
        fn next_id(&mut self) -> Uuid {
            self.0 += 1;
            Uuid::from_u128(self.0)
        }
    }

    fn queued(sim: &Sim, environment: &str, n: u128) -> deploy::Deployment {
        let repo_id: RepoId = sim.project().project.repos[0].repo_id;
        let mut ids = Ids(n * 100);
        let mut ctx = Ctx {
            now: Timestamp(n as i64 * 1_000),
            ids: &mut ids,
        };
        let events = deploy::queue(
            None,
            deploy::QueueRequest {
                project_id: sim.project_id,
                repo_id,
                environment: environment.into(),
                branch: "main".into(),
                cause: DeployCause::Redeploy,
                action: DeployActionKind::RunHost,
                target_url: None,
                on_failure: saka_core::vocab::OnFailure::Revert,
                actor: Some("operator:local".into()),
                reason: Some("again".into()),
                change_request_id: None,
            },
            &mut ctx,
        )
        .unwrap();
        events
            .iter()
            .fold(None, |d, e| {
                deploy::evolve(d, e, Timestamp(n as i64 * 1_000))
            })
            .unwrap()
    }

    fn step(d: &deploy::Deployment, input: Input) -> deploy::Deployment {
        let events = deploy::decide(d, input).unwrap();
        deploy::evolve_all(Some(d.clone()), &events, Timestamp(2_000)).unwrap()
    }

    #[test]
    fn deployments_are_listed_newest_first_and_read_with_their_evidence() {
        let sim = super::sim(&[raw("staging")], VERIFY);
        let first = queued(&sim, "staging", 1);
        let started = step(
            &first,
            Input::Start {
                sha: "a".repeat(40),
                previous_sha: None,
            },
        );
        let done = step(
            &started,
            Input::Fail {
                phase: DeployPhase::Verify,
                error: Coded {
                    code: "verify_failed".into(),
                    params: serde_json::json!({ "failed": 1, "total": 2 }),
                },
            },
        );
        let preview = queued(&sim, "staging", 2);
        let mut batch = Batch::new(Timestamp(5_000));
        batch.deployments.push(done.clone());
        batch.deployments.push(preview.clone());
        batch.deployment_checks.push((
            done.id,
            CheckRecord {
                phase: DeployPhase::Verify,
                ordinal: 1,
                command: Some("npm run e2e".into()),
                state: DeployCheckState::Failed,
                exit_code: Some(1),
                http_status: None,
                attempts: 1,
                duration_ms: 120,
                output_tail: "boom".into(),
                log: None,
                started_at: Timestamp(2_100),
                finished_at: Timestamp(2_220),
            },
        ));
        sim.engine().store().commit(&batch).unwrap();

        let page = |limit| Page { after: None, limit };
        let all = sim
            .engine()
            .deployments(sim.project_id, None, None, &page(10))
            .unwrap();
        let ids: Vec<_> = all.items.iter().map(|r| r.deployment.id).collect();
        assert_eq!(ids, [preview.id, done.id]);
        assert_eq!(all.items[0].repo.name, "shop");
        let one = sim
            .engine()
            .deployments(sim.project_id, None, None, &page(1))
            .unwrap();
        assert_eq!(one.items.len(), 1);
        assert!(one.next.is_some());
        let only_failed = sim
            .engine()
            .deployments(
                sim.project_id,
                Some("staging".into()),
                Some(DeploymentState::Failed),
                &page(10),
            )
            .unwrap();
        assert_eq!(only_failed.items.len(), 1);
        let none = sim
            .engine()
            .deployments(sim.project_id, Some("preview".into()), None, &page(10))
            .unwrap();
        assert!(none.items.is_empty());

        let detail = sim.engine().deployment(done.id).unwrap();
        assert_eq!(
            detail.row.deployment.error.as_ref().unwrap().code,
            "verify_failed"
        );
        assert_eq!(detail.checks.len(), 1);
        assert_eq!(detail.checks[0].output_tail, "boom");
        assert!(detail.merges.is_empty() && detail.tasks.is_empty());
        let views = sim.engine().environments(sim.project_id).unwrap();
        assert_eq!(views[0].queued.as_ref().unwrap().id, preview.id);
        assert!(views[0].active.is_none());
        assert!(
            sim.engine()
                .deployment(saka_core::ids::DeploymentId(Uuid::from_u128(9)))
                .is_err()
        );
    }
}
