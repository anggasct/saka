use serde_json::json;

use crate::event::Event;
use crate::job::{Job, JobKind};
use crate::model::Task;
use crate::task::LoopConfig;
use crate::time::Timestamp;
use crate::vocab::{EnvironmentState, PermissionState, Stage, StagePhase, WorkItemState};

const TEARDOWN_RETRY_INTERVAL_MS: i64 = 3_600_000;

pub fn react(task: &Task, event: &Event, cfg: &LoopConfig, now: Timestamp) -> Vec<Job> {
    use Event as E;
    let id = task.item.id;
    match event {
        E::StageRunCreated { .. } | E::StagePhaseChanged { .. } | E::WorkItemCancelled { .. } => {
            phase_jobs(task, cfg, now)
        }
        E::SessionTurnRequested {
            session_id,
            turn,
            prompt,
            ..
        } => vec![Job::new(
            JobKind::SessionSendTurn,
            format!("{session_id}:{turn}"),
            json!({ "work_item_id": id, "session_id": session_id, "turn": turn, "prompt": prompt }),
            now,
        )],
        E::SessionTurnEnded {
            session_id,
            turn,
            status,
            ..
        } if status == "success" => vec![Job::new(
            JobKind::EnvInspectWorktree,
            format!("{session_id}:{turn}"),
            json!({ "work_item_id": id, "session_id": session_id, "turn": turn, "environment_id": task.environment.as_ref().map(|e| e.id) }),
            now,
        )],
        E::SessionEnded {
            session_id,
            end_reason,
            ..
        } => {
            let mut jobs = Vec::new();
            if end_reason.code == "session_timeout" {
                jobs.push(Job::new(
                    JobKind::SessionInterrupt,
                    session_id,
                    json!({ "work_item_id": id, "session_id": session_id }),
                    now,
                ));
            }
            jobs.push(stop(task, *session_id, now));
            jobs
        }
        E::PermissionAnswered {
            permission_request_id,
            state: PermissionState::Allowed | PermissionState::Denied,
            ..
        } => task
            .permissions
            .iter()
            .filter(|p| p.id == *permission_request_id)
            .map(|p| deliver(task, p.id, now))
            .collect(),
        _ => vec![],
    }
}

fn stop(task: &Task, session_id: crate::ids::SessionId, now: Timestamp) -> Job {
    let unit = task
        .session
        .as_ref()
        .filter(|s| s.id == session_id)
        .map(|s| s.runner_unit.clone());
    Job::new(
        JobKind::SessionStop,
        session_id,
        json!({ "work_item_id": task.item.id, "session_id": session_id, "runner_unit": unit }),
        now,
    )
}

fn deliver(
    task: &Task,
    permission_request_id: crate::ids::PermissionRequestId,
    now: Timestamp,
) -> Job {
    let p = task
        .permissions
        .iter()
        .find(|p| p.id == permission_request_id);
    Job::new(
        JobKind::SessionDeliverPermissionAnswer,
        permission_request_id,
        json!({
            "work_item_id": task.item.id,
            "session_id": p.map(|p| p.session_id),
            "permission_request_id": permission_request_id,
            "cli_request_id": p.map(|p| p.cli_request_id.clone()),
            "behavior": if p.is_some_and(|p| p.state == PermissionState::Allowed) { "allow" } else { "deny" },
            "message": p.and_then(|p| p.message.clone()),
        }),
        now,
    )
}

pub fn phase_jobs(task: &Task, cfg: &LoopConfig, now: Timestamp) -> Vec<Job> {
    let item = &task.item;
    let id = item.id;
    let run = task.stage_run.as_ref();
    let run_id = run.map(|r| r.id);
    let mut jobs = Vec::new();

    if item.state.is_terminal() || item.state == WorkItemState::Blocked {
        if let Some(s) = task.live_session().filter(|_| item.state.is_terminal()) {
            jobs.push(stop(task, s.id, now));
        }
        match task.environment.as_ref() {
            Some(env)
                if item.state.is_terminal() && env.state == EnvironmentState::TeardownFailed =>
            {
                let hour = now.0 / TEARDOWN_RETRY_INTERVAL_MS;
                jobs.push(Job::new(
                    JobKind::EnvTeardown,
                    format!("{}:{hour}", env.id),
                    json!({ "work_item_id": id, "environment_id": env.id }),
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
                    json!({ "work_item_id": id, "environment_id": env.id }),
                    now,
                ));
            }
            _ => {}
        }
        return jobs;
    }

    if item.state == WorkItemState::Admitted && task.ready_environment().is_none() {
        if let Some(run_id) = run_id {
            jobs.push(Job::new(
                JobKind::EnvPrepare,
                run_id,
                json!({ "work_item_id": id, "stage_run_id": run_id, "repo_id": item.repo_id, "branch": item.branch }),
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
        (Stage::Dev, StagePhase::RunningSession) => {
            if let (Some(run_id), None) = (run_id, task.live_session()) {
                jobs.push(Job::new(
                    JobKind::SessionStart,
                    run_id,
                    json!({
                        "work_item_id": id,
                        "stage_run_id": run_id,
                        "environment_id": task.ready_environment().map(|e| e.id),
                    }),
                    now,
                ));
            }
            for p in &task.permissions {
                if matches!(p.state, PermissionState::Allowed | PermissionState::Denied)
                    && p.delivered_at.is_none()
                {
                    jobs.push(deliver(task, p.id, now));
                }
            }
        }
        (Stage::Dev, StagePhase::Pushing) => {
            if let (Some(run_id), Some(sha)) = (run_id, run.and_then(|r| r.head_sha.clone())) {
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
        (Stage::Dev, StagePhase::OpeningChange) => {
            jobs.push(Job::new(
                JobKind::ScmOpenChangeRequest,
                id,
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
                let bucket = now.0 / cfg.poll_interval_ms.max(1);
                jobs.push(Job::new(
                    JobKind::ScmPollChangeRequest,
                    format!("{}:{bucket}", cr.id),
                    json!({ "work_item_id": id, "change_request_id": cr.id, "repo_id": cr.repo_id, "number": cr.number }),
                    now,
                ));
            }
        }
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
        (Stage::Sync, StagePhase::Verifying) => {
            if let (Some(run_id), Some(cr)) = (run_id, cr) {
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
                    json!({ "work_item_id": id, "environment_id": env.id }),
                    now,
                ));
            }
            jobs.push(Job::new(
                JobKind::ScmDeleteBranch,
                id,
                json!({ "work_item_id": id, "repo_id": item.repo_id, "branch": item.branch }),
                now,
            ));
        }
        _ => {}
    }
    jobs
}
