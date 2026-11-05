#![allow(
    clippy::unwrap_used,
    reason = "test helpers fail loudly on a broken setup"
)]

use saka_core::Ctx;
use saka_core::event::Prompt;
use saka_core::external::{self, Incoming};
use saka_core::ids::{EnvironmentId, IdSource, ProjectId, RepoId, SessionId};
use saka_core::model::{Environment, RunOwner, Session, Task};
use saka_core::resume;
use saka_core::task::{self, run_view};
use saka_core::time::Timestamp;
use saka_core::vocab::{
    EnvironmentState, ExternalCause, ResumeCause, SessionState, Stage, Trigger, WorkflowPreset,
};
use saka_core::workflow::Workflow;
use uuid::Uuid;

struct Counter(u128);

impl IdSource for Counter {
    fn next_id(&mut self) -> Uuid {
        self.0 += 1;
        Uuid::from_u128(self.0)
    }
}

const NOW: Timestamp = Timestamp(1_000);

fn review_task() -> Task {
    let mut ids = Counter(100);
    let events = external::accept(
        &Workflow::of(WorkflowPreset::ReviewOnly),
        Incoming {
            project_id: ProjectId(Uuid::from_u128(1)),
            repo_id: RepoId(Uuid::from_u128(2)),
            repo_name: "shop".into(),
            number: 7,
            title: "Add retries".into(),
            url: "https://scm.test/acme/shop/pull/7".into(),
            head_branch: "feature/retries".into(),
            base_branch: "main".into(),
            head_sha: "a".repeat(40),
            author: "alice".into(),
            from_fork: false,
            draft: false,
            spec: None,
        },
        ExternalCause::Auto,
        None,
        &mut Ctx {
            now: NOW,
            ids: &mut ids,
        },
    )
    .unwrap();
    let mut task = None;
    for e in &events {
        task = task::evolve(task, e, NOW);
    }
    task.unwrap()
}

fn session(owner: RunOwner, env: EnvironmentId, state: SessionState) -> Session {
    Session {
        id: SessionId(Uuid::from_u128(50)),
        owner,
        environment_id: env,
        cli: "claude".into(),
        cli_version: None,
        cli_session_id: Some("cli-1".into()),
        model: None,
        account: "claude:default".into(),
        state,
        runner_unit: "unit".into(),
        socket_path: "/s".into(),
        log_path: "/l".into(),
        turns: 1,
        followups: 0,
        running_ms: 0,
        running_since: None,
        cost_usd_micros: None,
        input_tokens: None,
        output_tokens: None,
        end_reason: None,
        resumed_from_session_id: None,
        resume_cause: None,
        idle_since: None,
        interrupt_requested: None,
        steers: 0,
        last_offset: 0,
        recorded_turn: 0,
        recorded_running_ms: 0,
        started_at: Some(NOW),
        ended_at: None,
        created_at: NOW,
        updated_at: NOW,
    }
}

fn with_environment(task: &mut Task) -> EnvironmentId {
    let id = EnvironmentId(Uuid::from_u128(60));
    task.environment = Some(Environment {
        id,
        owner: task.run_owner().unwrap(),
        repo_id: task.item.repo_id,
        root_path: "/envs/e".into(),
        worktree_path: "/envs/e/worktree".into(),
        branch: None,
        base_sha: None,
        state: EnvironmentState::Ready,
        created_at: NOW,
        updated_at: NOW,
        port_start: None,
        port_count: None,
        recipe: None,
        recipe_sha256: None,
        toolchains: vec![],
        path_scope: vec![],
        docs: None,
    });
    id
}

#[test]
fn a_review_run_does_not_resume_the_session_of_another_run_even_on_a_manual_retry() {
    let mut task = review_task();
    let env = with_environment(&mut task);
    let dev_run = RunOwner::StageRun {
        work_item_id: task.item.id,
        stage_run_id: saka_core::ids::StageRunId(Uuid::from_u128(999)),
    };
    task.session = Some(session(dev_run, env, SessionState::Ended));
    task.stage_run.as_mut().unwrap().trigger = Trigger::Manual;
    assert_eq!(task.stage_run.as_ref().unwrap().stage, Stage::Review);

    let view = run_view(&task).unwrap();
    assert!(view.session.is_none());
    assert!(resume::plan(&view).is_none());
    assert!(resume::wants_session(&view));
    assert_eq!(view.packet, Prompt::ReviewPacket);
}

#[test]
fn a_review_run_resumes_its_own_lost_session() {
    let mut task = review_task();
    let env = with_environment(&mut task);
    let owner = task.run_owner().unwrap();
    task.session = Some(session(owner, env, SessionState::Lost));

    let view = run_view(&task).unwrap();
    let plan = resume::plan(&view).unwrap();
    assert_eq!(plan.cause, ResumeCause::Lost);
    assert_eq!(plan.cli_session_id.as_deref(), Some("cli-1"));
}
