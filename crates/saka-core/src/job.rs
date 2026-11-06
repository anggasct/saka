use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::ids::{SpecDraftId, WorkItemId};
use crate::model::RunOwner;
use crate::time::Timestamp;
use crate::vocab::{BlockerCode, StagePhase};

macro_rules! job_kinds {
    ($($variant:ident => $wire:literal, $attempts:literal;)*) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub enum JobKind {
            $(#[serde(rename = $wire)] $variant),*
        }

        impl JobKind {
            pub const ALL: &'static [JobKind] = &[$(JobKind::$variant),*];

            pub fn as_str(self) -> &'static str {
                match self { $(JobKind::$variant => $wire),* }
            }

            pub fn max_attempts(self) -> u32 {
                match self { $(JobKind::$variant => $attempts),* }
            }

            pub fn parse(s: &str) -> Option<JobKind> {
                match s { $($wire => Some(JobKind::$variant),)* _ => None }
            }
        }
    };
}

job_kinds! {
    EnvPrepare => "env.prepare", 3;
    EnvInspectWorktree => "env.inspect_worktree", 3;
    SessionStart => "session.start", 3;
    SessionSendTurn => "session.send_turn", 5;
    SessionDeliverPermissionAnswer => "session.deliver_permission_answer", 10;
    SessionInterrupt => "session.interrupt", 5;
    SessionStop => "session.stop", 5;
    SessionSteer => "session.steer", 5;
    SessionReattach => "session.reattach", 3;
    ScmPushBranch => "scm.push_branch", 5;
    ScmOpenChangeRequest => "scm.open_change_request", 5;
    ScmPollChangeRequest => "scm.poll_change_request", 5;
    ScmMerge => "scm.merge", 5;
    EnvVerifyMerged => "env.verify_merged", 3;
    ScmDeleteBranch => "scm.delete_branch", 5;
    EnvTeardown => "env.teardown", 5;
    ScmWebhookPrune => "scm.webhook_prune", 3;
    GateVerify => "gate.verify", 3;
    ScmReportStatus => "scm.report_status", 3;
    ScmPollCommit => "scm.poll_commit", 5;
    DraftLint => "draft.lint", 3;
    DraftCommit => "draft.commit", 5;
    WorkspaceReloadConfig => "workspace.reload_config", 3;
    ScmUserLinkPoll => "scm.user_link_poll", 3;
    ScmUserLinkRefresh => "scm.user_link_refresh", 3;
    ScmPushDocsBranch => "scm.push_docs_branch", 5;
    ScmOpenDocsChangeRequest => "scm.open_docs_change_request", 5;
    ScmMergeDocs => "scm.merge_docs", 5;
    HookRun => "hook.run", 3;
    ScmScanChanges => "scm.scan_changes", 3;
    EnvPrepareReview => "env.prepare_review", 3;
    ScmPostReview => "scm.post_review", 3;
}

pub fn exhausted_code(kind: JobKind) -> Option<(BlockerCode, Value)> {
    let scm = |phase: StagePhase| Some((BlockerCode::ScmUnavailable, json!({ "phase": phase })));
    match kind {
        JobKind::EnvPrepare | JobKind::EnvInspectWorktree | JobKind::EnvPrepareReview => {
            Some((BlockerCode::EnvSetupFailed, json!({})))
        }
        JobKind::SessionStart => Some((
            BlockerCode::SessionFailed,
            json!({ "reason": "launch_failed" }),
        )),
        JobKind::SessionSendTurn
        | JobKind::SessionDeliverPermissionAnswer
        | JobKind::SessionInterrupt
        | JobKind::SessionSteer => Some((
            BlockerCode::SessionLost,
            json!({ "cause": "runner_unreachable" }),
        )),
        JobKind::ScmPushBranch => scm(StagePhase::Pushing),
        JobKind::ScmOpenChangeRequest => scm(StagePhase::OpeningChange),
        JobKind::ScmPollChangeRequest => scm(StagePhase::AwaitingCi),
        JobKind::ScmMerge => scm(StagePhase::Merging),
        JobKind::ScmPostReview => scm(StagePhase::PostingReview),
        JobKind::ScmPushDocsBranch => Some((
            BlockerCode::ScmUnavailable,
            json!({ "phase": StagePhase::Pushing, "target": "docs" }),
        )),
        JobKind::ScmOpenDocsChangeRequest => Some((
            BlockerCode::ScmUnavailable,
            json!({ "phase": StagePhase::OpeningChange, "target": "docs" }),
        )),
        JobKind::ScmMergeDocs => scm(StagePhase::MergingDocs),
        JobKind::ScmPollCommit => scm(StagePhase::Verifying),
        JobKind::EnvVerifyMerged => Some((BlockerCode::DodFailed, json!({}))),
        JobKind::GateVerify => Some((
            BlockerCode::CiRed,
            json!({ "source": "saka", "reason": "runner_failed" }),
        )),
        JobKind::SessionStop
        | JobKind::SessionReattach
        | JobKind::ScmDeleteBranch
        | JobKind::EnvTeardown
        | JobKind::ScmWebhookPrune
        | JobKind::ScmReportStatus
        | JobKind::DraftLint
        | JobKind::DraftCommit
        | JobKind::WorkspaceReloadConfig
        | JobKind::ScmUserLinkPoll
        | JobKind::ScmUserLinkRefresh
        | JobKind::HookRun
        | JobKind::ScmScanChanges => None,
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Job {
    pub kind: JobKind,
    pub idempotency_key: String,
    pub payload: Value,
    pub run_at: Timestamp,
}

impl Job {
    pub fn new(
        kind: JobKind,
        key: impl std::fmt::Display,
        payload: Value,
        run_at: Timestamp,
    ) -> Job {
        Job {
            kind,
            idempotency_key: format!("{}:{key}", key_prefix(kind)),
            payload,
            run_at,
        }
    }

    pub fn max_attempts(&self) -> u32 {
        self.kind.max_attempts()
    }
}

pub fn owner_of(payload: &Value) -> Option<RunOwner> {
    serde_json::from_value(payload.get("owner")?.clone()).ok()
}

pub fn work_item_of(payload: &Value) -> Option<WorkItemId> {
    match payload.get("owner") {
        Some(_) => owner_of(payload)?.work_item_id(),
        None => serde_json::from_value(payload.get("work_item_id")?.clone()).ok(),
    }
}

pub fn spec_draft_of(payload: &Value) -> Option<SpecDraftId> {
    owner_of(payload)?.spec_draft_id()
}

fn key_prefix(kind: JobKind) -> &'static str {
    match kind {
        JobKind::EnvPrepare => "env.prepare",
        JobKind::EnvInspectWorktree => "env.inspect",
        JobKind::SessionStart => "session.start",
        JobKind::SessionSendTurn => "session.turn",
        JobKind::SessionDeliverPermissionAnswer => "perm.deliver",
        JobKind::SessionInterrupt => "session.interrupt",
        JobKind::SessionStop => "session.stop",
        JobKind::SessionSteer => "session.steer",
        JobKind::SessionReattach => "session.reattach",
        JobKind::ScmPushBranch => "scm.push",
        JobKind::ScmOpenChangeRequest => "scm.open_cr",
        JobKind::ScmPollChangeRequest => "scm.poll",
        JobKind::ScmMerge => "scm.merge",
        JobKind::EnvVerifyMerged => "sync.verify",
        JobKind::ScmDeleteBranch => "scm.delete_branch",
        JobKind::EnvTeardown => "env.teardown",
        JobKind::ScmWebhookPrune => "scm.webhook_prune",
        JobKind::GateVerify => "gate.verify",
        JobKind::ScmReportStatus => "scm.report_status",
        JobKind::ScmPollCommit => "scm.poll_commit",
        JobKind::DraftLint => "draft.lint",
        JobKind::DraftCommit => "draft.commit",
        JobKind::WorkspaceReloadConfig => "workspace.reload_config",
        JobKind::ScmUserLinkPoll => "scm.user_link_poll",
        JobKind::ScmUserLinkRefresh => "scm.user_link_refresh",
        JobKind::ScmPushDocsBranch => "scm.push_docs",
        JobKind::ScmOpenDocsChangeRequest => "scm.open_cr",
        JobKind::ScmMergeDocs => "scm.merge_docs",
        JobKind::HookRun => "hook",
        JobKind::ScmScanChanges => "scm.scan",
        JobKind::EnvPrepareReview => "env.prepare_review",
        JobKind::ScmPostReview => "scm.post_review",
    }
}
