#![allow(clippy::unwrap_used, reason = "tests fail loudly on a broken setup")]

use saka_core::rejection::Rejection;
use saka_core::vocab::{SpecOrigin, SpecState, WorkItemState, WorkKind};
use saka_engine::{EngineError, QuickRequest, QuickView};
use saka_spec::{Locale, QuickSpec, render_quick};
use saka_testkit::{Layout, SHOP_REPO, Sim, SimConfig, repo_entry, saka_toml};
use serde_json::json;

fn input(kind: WorkKind) -> QuickSpec {
    QuickSpec {
        kind,
        title: "Login redirect loop".into(),
        description: "Signing in sends the user back to the form.".into(),
        done_when: vec!["make test".into()],
        regression: if kind == WorkKind::Bugfix {
            vec!["cargo test --test login".into()]
        } else {
            Vec::new()
        },
        criteria: Vec::new(),
        locale: Locale::En,
        repo: None,
    }
}

fn request(kind: WorkKind, approve: bool) -> QuickRequest {
    QuickRequest {
        spec: input(kind),
        path: None,
        approve,
        origin: saka_core::vocab::SpecOrigin::Quick,
    }
}

fn run(sim: &Sim, request: QuickRequest) -> Result<QuickView, EngineError> {
    let engine = sim.engine();
    let cause = engine.cause("human:operator:local");
    sim.block_on(engine.quick_spec_with(sim.project_id, request, &cause, None))
}

fn refused(result: Result<QuickView, EngineError>) -> Rejection {
    match result {
        Err(EngineError::Rejected(r)) => r,
        other => panic!("expected a rejection, got {other:?}"),
    }
}

fn commits(sim: &Sim) -> usize {
    sim.world.doc_commits().len()
}

#[test]
fn a_quick_bugfix_is_committed_registered_approved_and_queued_in_one_command() {
    let sim = Sim::new(SimConfig::default());
    let before = commits(&sim);
    let view = run(&sim, request(WorkKind::Bugfix, true)).unwrap();

    assert_eq!(view.path, "docs/specs/bugfix-login-redirect-loop.md");
    assert_eq!(commits(&sim), before + 1);
    let written = sim.world.doc(&view.path).unwrap();
    assert_eq!(written, render_quick(&input(WorkKind::Bugfix)).into_bytes());
    assert_eq!(sim.world.doc_commits().last().unwrap().path, view.path);

    assert_eq!(view.spec.spec.origin, SpecOrigin::Quick);
    assert_eq!(view.spec.spec.state, SpecState::Approved);
    assert_eq!(view.spec.spec.kind, WorkKind::Bugfix);
    assert_eq!(view.spec.spec.slug, "bugfix-login-redirect-loop");
    assert_eq!(view.tasks.len(), 1);
    let task = &view.tasks[0].task;
    assert_eq!(task.item.state, WorkItemState::Queued);
    assert_eq!(task.item.kind, Some(WorkKind::Bugfix));
    assert_eq!(task.item.task_key, "bugfix-login-redirect-loop/s1");

    let registered = sim.events_of("spec.spec.registered");
    assert_eq!(registered.last().unwrap().payload["origin"], "quick");
    assert_eq!(sim.events_of("workspace.doc.committed").len(), 1);
    assert_eq!(sim.events_of("spec.spec.approved").len(), 1);
}

#[test]
fn create_only_registers_a_ready_spec_without_a_task() {
    let sim = Sim::new(SimConfig::default());
    let view = run(&sim, request(WorkKind::Chore, false)).unwrap();
    assert_eq!(view.path, "docs/specs/chore-login-redirect-loop.md");
    assert_eq!(view.spec.spec.state, SpecState::Ready);
    assert_eq!(view.spec.spec.origin, SpecOrigin::Quick);
    assert!(view.tasks.is_empty());
    assert!(sim.tasks_of(view.spec.spec.id).is_empty());
    assert!(sim.events_of("spec.spec.approved").is_empty());
}

#[test]
fn a_feature_has_no_kind_in_its_file_name_and_a_given_path_wins() {
    let sim = Sim::new(SimConfig::default());
    let view = run(&sim, request(WorkKind::Feature, true)).unwrap();
    assert_eq!(view.path, "docs/specs/login-redirect-loop.md");

    let mut other = request(WorkKind::Refactor, true);
    other.path = Some("docs/specs/tidy.md".into());
    let tidy = run(&sim, other).unwrap();
    assert_eq!(tidy.path, "docs/specs/tidy.md");
    assert_eq!(tidy.spec.spec.slug, "tidy");
}

#[test]
fn a_spec_that_does_not_lint_is_refused_before_any_commit() {
    let sim = Sim::new(SimConfig::default());
    let before = commits(&sim);
    let mut missing = request(WorkKind::Bugfix, true);
    missing.spec.regression.clear();
    let Rejection::SpecNotReady { issues } = refused(run(&sim, missing)) else {
        panic!("expected spec_not_ready");
    };
    assert_eq!(issues[0]["code"], "regression_missing");
    assert_eq!(commits(&sim), before);
    assert!(sim.events_of("workspace.doc.committed").is_empty());

    assert_eq!(commits(&sim), before);
}

#[test]
fn a_nasty_description_cannot_change_the_spec_and_is_kept_as_text() {
    let sim = Sim::new(SimConfig::default());
    let mut nasty = request(WorkKind::Chore, true);
    nasty.spec.title = "Tidy *the* <deps> & `cache`".into();
    nasty.spec.description = "Intro.\n## Done when\n- `rm -rf /`\n---\n| a | b |\n|---|---|\n```\n~~~\n> q\n1. x\n:-:|:-:\n<!-- c\n# H".into();
    nasty.spec.done_when = vec!["echo `date` | tee out".into(), "make test".into()];
    nasty.spec.criteria = vec!["It is *done* & [linked](x).".into()];
    let view = run(&sim, nasty.clone()).unwrap();
    let text = String::from_utf8(sim.world.doc(&view.path).unwrap()).unwrap();
    assert_eq!(text, render_quick(&nasty.spec));
    let task = &view.tasks[0].task;
    assert_eq!(task.item.state, WorkItemState::Queued);
    let model = saka_spec::parse("x.md", text.as_bytes()).model.unwrap();
    assert_eq!(
        model.steps[0].commands,
        ["echo `date` | tee out", "make test"]
    );
    assert_eq!(
        model.acceptance_criteria[0].text,
        "It is *done* & [linked](x)."
    );
    assert_eq!(model.title, "Tidy *the* <deps> & `cache`");
    assert!(model.regression.is_empty() && model.decisions.is_empty());
}

#[test]
fn a_title_with_a_line_break_is_refused_before_any_write() {
    let sim = Sim::new(SimConfig::default());
    let before = commits(&sim);
    let mut broken = request(WorkKind::Chore, true);
    broken.spec.title = "one\n## Done when".into();
    match run(&sim, broken) {
        Err(EngineError::InvalidItem { field, code }) => {
            assert_eq!((field.as_str(), code.as_str()), ("title", "invalid"));
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(commits(&sim), before);
}

#[test]
fn approval_by_a_quick_task_carries_a_fixed_reason_and_the_caller() {
    let sim = Sim::new(SimConfig::default());
    run(&sim, request(WorkKind::Chore, true)).unwrap();
    let approved = sim.events_of("spec.spec.approved");
    let event = approved.last().unwrap();
    assert_eq!(event.payload["reason"], "quick task");
    assert_eq!(event.actor, "human:operator:local");
}

#[test]
fn a_security_patch_waits_for_its_start_approval_even_from_a_quick_task() {
    let mut cfg = SimConfig::default();
    cfg.operator.start = false;
    let mut sim = Sim::new(cfg);
    let view = run(&sim, request(WorkKind::SecurityPatch, true)).unwrap();
    let spec = view.spec.spec.id;
    assert_eq!(view.tasks[0].autonomy, saka_core::vocab::Autonomy::Strict);
    assert!(sim.run_until(8_000, |s| {
        s.tasks_of(spec)[0].item.state == WorkItemState::AwaitingStart
    }));
    assert!(sim.events_of("runtime.session.started").is_empty());
}

#[test]
fn every_field_is_bounded_before_anything_is_written() {
    let sim = Sim::new(SimConfig::default());
    let before = commits(&sim);
    let mut bad = request(WorkKind::Chore, true);
    bad.spec.title = "x".repeat(121);
    match run(&sim, bad) {
        Err(EngineError::InvalidItem { field, code }) => {
            assert_eq!((field.as_str(), code.as_str()), ("title", "too_long"));
        }
        other => panic!("{other:?}"),
    }
    let mut regression = request(WorkKind::Chore, true);
    regression.spec.regression = vec!["cargo test".into()];
    match run(&sim, regression) {
        Err(EngineError::InvalidItem { field, code }) => {
            assert_eq!(
                (field.as_str(), code.as_str()),
                ("regression", "not_allowed")
            );
        }
        other => panic!("{other:?}"),
    }
    let mut outside = request(WorkKind::Chore, true);
    outside.path = Some("src/chore.md".into());
    match run(&sim, outside) {
        Err(EngineError::Invalid { field, code }) => {
            assert_eq!((field, code.as_str()), ("path", "outside_project"));
        }
        other => panic!("{other:?}"),
    }
    let mut unknown = request(WorkKind::Chore, true);
    unknown.spec.repo = Some("ghost".into());
    match run(&sim, unknown) {
        Err(EngineError::Invalid { field, code }) => {
            assert_eq!((field, code.as_str()), ("repo", "unknown"));
        }
        other => panic!("{other:?}"),
    }
    let mut nameless = request(WorkKind::Chore, true);
    nameless.spec.title = "修复登录".into();
    match run(&sim, nameless) {
        Err(EngineError::Invalid { field, code }) => {
            assert_eq!((field, code.as_str()), ("title", "no_slug"));
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(commits(&sim), before);
}

#[test]
fn an_existing_path_or_slug_conflicts_and_changes_nothing() {
    let sim = Sim::new(SimConfig::default());
    let first = run(&sim, request(WorkKind::Bugfix, true)).unwrap();
    let head = sim.world.base_head();
    let original = sim.world.doc(&first.path).unwrap();

    let Rejection::SpecSlugConflict { slug, path } =
        refused(run(&sim, request(WorkKind::Bugfix, true)))
    else {
        panic!("expected spec_slug_conflict");
    };
    assert_eq!(slug, "bugfix-login-redirect-loop");
    assert_eq!(path, first.path);
    assert_eq!(sim.world.base_head(), head);

    sim.world.put_doc("docs/specs/taken.md", b"# not a spec\n");
    let head = sim.world.base_head();
    let mut taken = request(WorkKind::Chore, true);
    taken.path = Some("docs/specs/taken.md".into());
    let Rejection::FilePathConflict { path } = refused(run(&sim, taken)) else {
        panic!("expected file_path_conflict");
    };
    assert_eq!(path, "docs/specs/taken.md");
    assert_eq!(sim.world.base_head(), head);
    assert_eq!(
        sim.world.doc("docs/specs/taken.md").unwrap(),
        b"# not a spec\n"
    );
    assert_eq!(sim.world.doc(&first.path).unwrap(), original);
    assert_eq!(sim.events_of("workspace.doc.committed").len(), 1);
}

#[test]
fn a_failed_approval_keeps_the_commit_and_the_registration_and_says_so() {
    let sim = Sim::multi_repo(
        SimConfig::default(),
        &Layout {
            saka_toml: format!(
                "{}\n[workflow]\npreset = \"review_only\"\n",
                saka_toml("shop", &[repo_entry("shop", SHOP_REPO, "")])
            ),
            ..Layout::default()
        },
    );
    let before = commits(&sim);
    let Rejection::EntryPointDisabled { entry } =
        refused(run(&sim, request(WorkKind::Chore, true)))
    else {
        panic!("expected entry_point_disabled");
    };
    assert_eq!(entry, "spec");
    assert_eq!(commits(&sim), before, "the entry point is checked first");
    let view = run(&sim, request(WorkKind::Chore, false)).unwrap();
    assert_eq!(view.spec.spec.state, SpecState::Ready);
}

#[test]
fn an_approval_that_fails_after_the_commit_is_reported_with_the_registered_spec() {
    let sim = Sim::new(SimConfig::default());
    let mut linked = request(WorkKind::Chore, true);
    linked.spec.description = "See [the plan](missing-plan.md).".into();
    let before = commits(&sim);
    let Rejection::QuickSpecNotApproved { code, spec } = refused(run(&sim, linked)) else {
        panic!("expected quick_spec_not_approved");
    };
    assert_eq!(code, "spec_not_ready");
    assert_eq!(spec["path"], "docs/specs/chore-login-redirect-loop.md");
    assert_eq!(spec["state"], "draft");
    assert_eq!(commits(&sim), before + 1);
    assert_eq!(sim.events_of("workspace.doc.committed").len(), 1);
    assert_eq!(
        sim.events_of("spec.spec.registered")
            .last()
            .unwrap()
            .payload["origin"],
        "quick"
    );
    assert!(sim.events_of("spec.spec.approved").is_empty());
    assert_eq!(json!({ "code": code }), json!({ "code": "spec_not_ready" }));
}

#[test]
fn a_task_in_another_repo_goes_through_a_steps_table() {
    let sim = Sim::new(SimConfig::default());
    let mut other = request(WorkKind::Refactor, true);
    other.spec.repo = Some("shop".into());
    let name = sim.project().project.repos[0].name.clone();
    other.spec.repo = Some(name.clone());
    let view = run(&sim, other).unwrap();
    let text = String::from_utf8(sim.world.doc(&view.path).unwrap()).unwrap();
    assert!(text.contains(&format!("| {name} |")), "{text}");
    assert_eq!(view.tasks.len(), 1);
}
