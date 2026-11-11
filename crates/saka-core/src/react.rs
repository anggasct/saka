use serde_json::json;

use crate::event::Event;
use crate::job::{Job, JobKind};
use crate::model::Task;
use crate::resume;
use crate::session;
use crate::task::{
    LoopConfig, docs_pending, gate_policy, needs_docs_change, run_view, watches_change,
    watches_docs_change,
};
use crate::time::Timestamp;
use crate::vocab::{
    BlockerCode, ChangeRole, ChangeState, DecisionState, EnvironmentState, GateOutcome, GateSource,
    PermissionState, Stage, StagePhase, WorkItemState,
};

const TEARDOWN_RETRY_INTERVAL_MS: i64 = 3_600_000;

pub fn react(task: &Task, event: &Event, cfg: &LoopConfig, now: Timestamp) -> Vec<Job> {
    use Event as E;
    match event {
        E::StageRunCreated { .. }
        | E::StagePhaseChanged { .. }
        | E::WorkItemCancelled { .. }
        | E::BranchPushed {
            target: ChangeRole::Docs,
            ..
        }
        | E::ChangeRequestUpdated {
            role: ChangeRole::Docs,
            ..
        } => phase_jobs(task, cfg, now),
        E::SessionTurnEnded {
            owner,
            session_id,
            turn,
            status,
            ..
        } if status == "success"
            && task
                .stage_run
                .as_ref()
                .is_none_or(|r| r.stage != Stage::Review)
            && task
                .live_session()
                .is_some_and(|s| s.id == *session_id && s.interrupt_requested.is_none())
            && !run_view(task).is_some_and(|v| v.run_decisions().any(|d| d.is_open())) =>
        {
            vec![Job::new(
                JobKind::EnvInspectWorktree,
                format!("{session_id}:{turn}"),
                json!({ "owner": owner, "session_id": session_id, "turn": turn, "environment_id": task.environment.as_ref().map(|e| e.id) }),
                now,
            )]
        }
        E::GateStarted {
            gate_result_id,
            source: GateSource::Saka,
            ..
        }
        | E::GateObserved {
            gate_result_id,
            source: GateSource::Saka,
            ..
        } => report_status(task, *gate_result_id, cfg, now)
            .into_iter()
            .chain(phase_jobs(task, cfg, now))
            .collect(),
        E::ChangeRequestUpdateRequested {
            work_item_id,
            change_request_id,
            expected_head_sha,
        } => task
            .change_request
            .as_ref()
            .filter(|cr| cr.id == *change_request_id)
            .map(|cr| {
                Job::new(
                    JobKind::ScmUpdateBranch,
                    format!(
                        "{work_item_id}:{}:{expected_head_sha}",
                        task.stage_run
                            .as_ref()
                            .map_or_else(String::new, |r| r.id.to_string())
                    ),
                    json!({
                        "work_item_id": work_item_id,
                        "repo_id": cr.repo_id,
                        "number": cr.number,
                        "base_branch": cr.base_branch,
                        "expected_head_sha": expected_head_sha,
                    }),
                    now,
                )
            })
            .into_iter()
            .collect(),
        _ => {
            let mut jobs = run_view(task)
                .map(|view| session::react(&view, event, now))
                .unwrap_or_default();
            jobs.extend(blocker_comment(task, event, now));
            jobs
        }
    }
}

pub fn comment_job(
    task: &Task,
    head_sha: &str,
    code: &str,
    params: serde_json::Value,
    now: Timestamp,
) -> Option<Job> {
    let cr = task.change_request.as_ref()?;
    Some(Job::new(
        JobKind::ScmPostComment,
        format!("{}:{head_sha}:{code}", task.item.id),
        json!({
            "work_item_id": task.item.id,
            "repo_id": cr.repo_id,
            "number": cr.number,
            "head_sha": head_sha,
            "code": code,
            "params": params,
        }),
        now,
    ))
}

fn blocker_comment(task: &Task, event: &Event, now: Timestamp) -> Option<Job> {
    let Event::WorkItemBlocked { code, params, .. } = event else {
        return None;
    };
    task.external.as_ref()?;
    let head = params.get("head_sha").and_then(serde_json::Value::as_str)?;
    let reason = params.get("reason").and_then(serde_json::Value::as_str);
    let comment = match (
        code,
        params.get("updatable").and_then(serde_json::Value::as_bool),
    ) {
        (BlockerCode::ChangeBehindBase, Some(false)) => BlockerCode::ChangeBehindBase.as_str(),
        (BlockerCode::MergeConflict, _) if reason == Some("update_conflict") => "update_conflict",
        _ => return None,
    };
    comment_job(task, head, comment, params.clone(), now)
}

fn report_status(
    task: &Task,
    gate_result_id: crate::ids::GateResultId,
    cfg: &LoopConfig,
    now: Timestamp,
) -> Option<Job> {
    let gate = task.gates.iter().find(|g| g.id == gate_result_id)?;
    let cr = task.change_request.as_ref()?;
    if !cfg.commit_status {
        return None;
    }
    let (state, description) = match gate.outcome {
        GateOutcome::Pending => ("pending", "Running the Done when commands".to_string()),
        GateOutcome::Passed => ("success", format!("{} commands passed", gate.checks.len())),
        GateOutcome::Failed => {
            let failed = gate
                .checks
                .iter()
                .find(|c| c.is_failure())
                .map_or_else(|| "saka/verify".to_string(), |c| c.name.clone());
            ("failure", format!("`{failed}` failed"))
        }
    };
    let description: String = if description.chars().count() > 140 {
        let cut: String = description.chars().take(139).collect();
        format!("{cut}…")
    } else {
        description
    };
    Some(Job::new(
        JobKind::ScmReportStatus,
        gate_result_id,
        json!({
            "work_item_id": task.item.id,
            "repo_id": cr.repo_id,
            "sha": gate.head_sha,
            "state": state,
            "description": description,
        }),
        now,
    ))
}

pub fn phase_jobs(task: &Task, cfg: &LoopConfig, now: Timestamp) -> Vec<Job> {
    let item = &task.item;
    let id = item.id;
    let run = task.stage_run.as_ref();
    let run_id = run.map(|r| r.id);
    let mut jobs = Vec::new();
    if watches_change(task) {
        jobs.extend(poll_job(task.change_request.as_ref(), task, cfg, now));
    }
    if watches_docs_change(task) {
        jobs.extend(poll_job(task.docs_change.as_ref(), task, cfg, now));
    }

    if let Some(env) = task.environment.as_ref().filter(|e| {
        e.state == EnvironmentState::Ready
            && task.external.is_some()
            && !task.is_at(Stage::Review, StagePhase::PreparingEnvironment)
            && !task.is_at(Stage::Review, StagePhase::RunningSession)
            && !task.is_at_stage(Stage::Fix)
    }) {
        jobs.push(Job::new(
            JobKind::EnvTeardown,
            env.id,
            json!({ "owner": env.owner, "environment_id": env.id }),
            now,
        ));
    }

    if item.state.is_terminal() || item.state == WorkItemState::Blocked {
        if let Some(s) = task.live_session().filter(|_| item.state.is_terminal()) {
            jobs.push(session::stop_job(s, now));
        }
        match task.environment.as_ref() {
            Some(env)
                if item.state.is_terminal() && env.state == EnvironmentState::TeardownFailed =>
            {
                let hour = now.0 / TEARDOWN_RETRY_INTERVAL_MS;
                jobs.push(Job::new(
                    JobKind::EnvTeardown,
                    format!("{}:{hour}", env.id),
                    json!({ "owner": env.owner, "environment_id": env.id }),
                    now,
                ));
            }
            Some(env)
                if item.state == WorkItemState::Cancelled
                    && env.state != EnvironmentState::TornDown =>
            {
                jobs.push(Job::new(
                    JobKind::EnvTeardown,
                    env.id,
                    json!({ "owner": env.owner, "environment_id": env.id }),
                    now,
                ));
            }
            _ => {}
        }
        return jobs;
    }

    if item.state == WorkItemState::Admitted && task.ready_environment().is_none() {
        if let (Some(run_id), Some(owner)) = (run_id, task.run_owner()) {
            jobs.push(Job::new(
                JobKind::EnvPrepare,
                run_id,
                json!({ "owner": owner, "repo_id": item.repo_id, "branch": item.branch }),
                now,
            ));
        }
        return jobs;
    }

    let (Some(stage), Some(phase)) = (item.stage, item.stage_phase) else {
        return jobs;
    };
    let cr = task.change_request.as_ref();
    match (stage, phase) {
        (Stage::Dev | Stage::Fix | Stage::Review, StagePhase::RunningSession) => {
            jobs.extend(session_jobs(task, now));
        }
        (Stage::Review, StagePhase::PreparingEnvironment) => {
            if let (Some(run), Some(cr)) = (run, cr) {
                jobs.push(Job::new(
                    JobKind::EnvPrepareReview,
                    run.id,
                    json!({
                        "work_item_id": id,
                        "stage_run_id": run.id,
                        "repo_id": cr.repo_id,
                        "number": cr.number,
                        "base_branch": cr.base_branch,
                        "head_sha": run.head_sha,
                    }),
                    now,
                ));
            }
        }
        (Stage::Review, StagePhase::PostingReview) => {
            if let (Some(run), Some(cr), Some(review)) = (run, cr, task.unposted_review()) {
                jobs.push(Job::new(
                    JobKind::ScmPostReview,
                    format!("{}:{}", run.id, review.id),
                    json!({
                        "work_item_id": id,
                        "stage_run_id": run.id,
                        "review_id": review.id,
                        "repo_id": cr.repo_id,
                        "number": cr.number,
                        "head_sha": review.head_sha,
                    }),
                    now,
                ));
            }
        }
        (Stage::Dev | Stage::Fix, StagePhase::Pushing) => {
            let docs = task.environment.as_ref().and_then(|e| e.docs.as_ref());
            if let (Some(run_id), Some(sha), Some(docs)) = (run_id, docs_pending(task), docs) {
                jobs.push(Job::new(
                    JobKind::ScmPushDocsBranch,
                    format!("{run_id}:{sha}"),
                    json!({
                        "work_item_id": id,
                        "stage_run_id": run_id,
                        "repo_id": docs.repo_id,
                        "branch": docs.branch,
                        "local_sha": sha,
                        "expected_remote_sha": docs.pushed_sha,
                    }),
                    now,
                ));
            } else if let (Some(run_id), Some(sha)) = (run_id, run.and_then(|r| r.head_sha.clone()))
            {
                jobs.push(Job::new(
                    JobKind::ScmPushBranch,
                    format!("{run_id}:{sha}"),
                    json!({
                        "work_item_id": id,
                        "stage_run_id": run_id,
                        "repo_id": item.repo_id,
                        "branch": item.branch,
                        "local_sha": sha,
                        "expected_remote_sha": cr.map(|c| c.head_sha.clone()),
                        "environment_id": task.environment.as_ref().map(|e| e.id),
                    }),
                    now,
                ));
            }
        }
        (Stage::Dev | Stage::Fix, StagePhase::OpeningChange) if cr.is_some() => {
            if needs_docs_change(task) {
                let run = run_id.map(|r| r.to_string()).unwrap_or_default();
                jobs.push(Job::new(
                    JobKind::ScmOpenDocsChangeRequest,
                    format!("{id}:docs:{run}"),
                    json!({ "work_item_id": id }),
                    now,
                ));
            }
        }
        (Stage::Dev | Stage::Fix, StagePhase::OpeningChange) => {
            // Keyed per run: a retry into this phase opens the change again, with the credential
            // the project's mode names at that moment.
            let key = run_id.map_or_else(|| id.to_string(), |run| format!("{id}:{run}"));
            jobs.push(Job::new(
                JobKind::ScmOpenChangeRequest,
                key,
                json!({
                    "work_item_id": id,
                    "repo_id": item.repo_id,
                    "branch": item.branch,
                    "head_sha": run.and_then(|r| r.head_sha.clone()),
                }),
                now,
            ));
        }
        (Stage::Merge, StagePhase::AwaitingCi | StagePhase::AwaitingMerge) => {
            if let Some(cr) = cr {
                let policy = gate_policy(task);
                let unsettled = task
                    .gate_for(GateSource::Saka, &cr.head_sha)
                    .is_none_or(|g| g.outcome == GateOutcome::Pending);
                if let (true, true, Some(run)) = (policy.runs_saka(), unsettled, run) {
                    jobs.push(Job::new(
                        JobKind::GateVerify,
                        format!("{}:{}", run.id, cr.head_sha),
                        json!({ "work_item_id": id, "stage_run_id": run.id, "head_sha": cr.head_sha }),
                        now,
                    ));
                }
            }
        }
        (Stage::Merge, StagePhase::Integrating) => {
            if let (Some(cr), Some(run)) = (cr, run) {
                jobs.push(Job::new(
                    JobKind::ScmIntegrate,
                    format!("{}:{}", run.id, cr.head_sha),
                    json!({
                        "work_item_id": id,
                        "stage_run_id": run.id,
                        "repo_id": cr.repo_id,
                        "number": cr.number,
                        "base_branch": cr.base_branch,
                        "head_sha": cr.head_sha,
                    }),
                    now,
                ));
            }
        }
        (Stage::Merge, StagePhase::Merging) if run.is_some_and(|r| r.queue_native) => {}
        (Stage::Merge, StagePhase::Merging) => {
            if let Some(cr) = cr {
                jobs.push(Job::new(
                    JobKind::ScmMerge,
                    format!("{id}:{}", cr.head_sha),
                    json!({
                        "work_item_id": id,
                        "change_request_id": cr.id,
                        "repo_id": cr.repo_id,
                        "number": cr.number,
                        "head_sha": cr.head_sha,
                        "merge_method": cfg.merge_method,
                    }),
                    now,
                ));
            }
        }
        (Stage::Sync, StagePhase::MergingDocs) => {
            if let (Some(docs), Some(run)) = (&task.docs_change, run_id) {
                // Keyed by run: a retried merge after a refusal names the same head again.
                jobs.push(Job::new(
                    JobKind::ScmMergeDocs,
                    format!("{id}:{run}:{}", docs.head_sha),
                    json!({
                        "work_item_id": id,
                        "change_request_id": docs.id,
                        "repo_id": docs.repo_id,
                        "number": docs.number,
                        "head_sha": docs.head_sha,
                    }),
                    now,
                ));
            }
        }
        (Stage::Sync, StagePhase::Verifying) => {
            if let (Some(run), Some(cr), Some(target)) =
                (run, cr, run.and_then(|r| r.head_sha.as_ref()))
            {
                let policy = gate_policy(task);
                let unsettled = |source| {
                    task.gate_for(source, target)
                        .is_none_or(|g| g.outcome == GateOutcome::Pending)
                };
                if policy.runs_saka() && unsettled(GateSource::Saka) {
                    jobs.push(Job::new(
                        JobKind::GateVerify,
                        format!("{}:{target}", run.id),
                        json!({ "work_item_id": id, "stage_run_id": run.id, "head_sha": target }),
                        now,
                    ));
                }
                if policy.uses(GateSource::Scm) && unsettled(GateSource::Scm) {
                    let bucket = now.0 / cfg.poll_interval_ms.max(1);
                    jobs.push(Job::new(
                        JobKind::ScmPollCommit,
                        format!("{}:{target}:{bucket}", run.id),
                        json!({ "work_item_id": id, "stage_run_id": run.id, "repo_id": cr.repo_id, "sha": target }),
                        now,
                    ));
                }
            } else if let (Some(run_id), Some(cr)) = (run_id, cr) {
                jobs.push(Job::new(
                    JobKind::EnvVerifyMerged,
                    format!("{run_id}:1"),
                    json!({
                        "work_item_id": id,
                        "repo_id": cr.repo_id,
                        "merge_commit_sha": cr.merge_commit_sha,
                        "base_branch": cr.base_branch,
                    }),
                    now,
                ));
            }
        }
        (Stage::Sync, StagePhase::TearingDown) => {
            if let Some(env) = task
                .environment
                .as_ref()
                .filter(|e| e.state != EnvironmentState::TornDown)
            {
                jobs.push(Job::new(
                    JobKind::EnvTeardown,
                    env.id,
                    json!({ "owner": env.owner, "environment_id": env.id }),
                    now,
                ));
            }
            if cfg.delete_branch_after_merge && task.external.is_none() {
                jobs.push(Job::new(
                    JobKind::ScmDeleteBranch,
                    id,
                    json!({ "work_item_id": id, "repo_id": item.repo_id, "branch": item.branch }),
                    now,
                ));
                let finished = |c: &&crate::model::ChangeRequest| {
                    c.state == ChangeState::Merged || c.skipped_at.is_some()
                };
                if let Some(docs) = task.docs_change.as_ref().filter(finished) {
                    jobs.push(Job::new(
                        JobKind::ScmDeleteBranch,
                        format!("{id}:docs"),
                        json!({ "work_item_id": id, "repo_id": docs.repo_id, "branch": docs.head_branch }),
                        now,
                    ));
                }
            }
        }
        _ => {}
    }
    jobs
}

fn session_jobs(task: &Task, now: Timestamp) -> Vec<Job> {
    let mut jobs = Vec::new();
    let run_id = task.stage_run.as_ref().map(|r| r.id);
    let view = run_view(task);
    if let (Some(run_id), Some(view)) = (run_id, view.as_ref().filter(|v| resume::wants_session(v)))
    {
        let plan = resume::plan(view);
        let key = match plan
            .as_ref()
            .filter(|p| p.cause == crate::vocab::ResumeCause::Lost)
        {
            Some(p) => format!("{run_id}:{}", p.from),
            None => run_id.to_string(),
        };
        jobs.push(Job::new(
            JobKind::SessionStart,
            key,
            json!({
                "owner": view.owner,
                "environment_id": view.ready_environment,
                "resume": plan.map(|p| json!({
                    "session_id": p.from,
                    "cause": p.cause,
                    "cli_session_id": p.cli_session_id,
                })),
            }),
            now,
        ));
    }
    for p in &task.permissions {
        if matches!(p.state, PermissionState::Allowed | PermissionState::Denied)
            && p.delivered_at.is_none()
        {
            jobs.push(session::deliver_job(p, now));
        }
    }
    if let Some(view) = view.as_ref() {
        jobs.extend(
            view.run_decisions()
                .filter(|d| d.state == DecisionState::Answered)
                .map(|d| session::deliver_decision_job(d, now)),
        );
    }
    jobs
}

fn poll_job(
    cr: Option<&crate::model::ChangeRequest>,
    task: &Task,
    cfg: &LoopConfig,
    now: Timestamp,
) -> Option<Job> {
    let cr = cr?;
    let bucket = now.0 / cfg.poll_interval_ms.max(1);
    Some(Job::new(
        JobKind::ScmPollChangeRequest,
        format!("{}:{bucket}", cr.id),
        json!({ "work_item_id": task.item.id, "change_request_id": cr.id, "repo_id": cr.repo_id, "number": cr.number }),
        now,
    ))
}
