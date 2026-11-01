//! Side effects as data. The engine stores each job in the same transaction as the
//! events that caused it; a worker runs it through a port and reports the result back.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

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
    ScmPushBranch => "scm.push_branch", 5;
    ScmOpenChangeRequest => "scm.open_change_request", 5;
    ScmPollChangeRequest => "scm.poll_change_request", 5;
    ScmMerge => "scm.merge", 5;
    EnvVerifyMerged => "env.verify_merged", 1;
    ScmDeleteBranch => "scm.delete_branch", 5;
    EnvTeardown => "env.teardown", 5;
}

impl JobKind {
    /// The blocker a job leaves behind when it runs out of attempts; `None` for cleanup
    /// jobs, which the reconciler simply schedules again.
    pub fn exhausted_blocker(self) -> Option<(BlockerCode, Value)> {
        let scm =
            |phase: StagePhase| Some((BlockerCode::ScmUnavailable, json!({ "phase": phase })));
        match self {
            JobKind::EnvPrepare | JobKind::EnvInspectWorktree => {
                Some((BlockerCode::EnvSetupFailed, json!({})))
            }
            JobKind::SessionStart => Some((
                BlockerCode::SessionFailed,
                json!({ "reason": "launch_failed" }),
            )),
            JobKind::SessionSendTurn
            | JobKind::SessionDeliverPermissionAnswer
            | JobKind::SessionInterrupt => Some((
                BlockerCode::SessionLost,
                json!({ "cause": "runner_unreachable" }),
            )),
            JobKind::ScmPushBranch => scm(StagePhase::Pushing),
            JobKind::ScmOpenChangeRequest => scm(StagePhase::OpeningChange),
            JobKind::ScmPollChangeRequest => scm(StagePhase::AwaitingCi),
            JobKind::ScmMerge => scm(StagePhase::Merging),
            JobKind::EnvVerifyMerged => Some((BlockerCode::DodFailed, json!({}))),
            JobKind::SessionStop | JobKind::ScmDeleteBranch | JobKind::EnvTeardown => None,
        }
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

/// Idempotency key prefixes, as documented for each job kind.
fn key_prefix(kind: JobKind) -> &'static str {
    match kind {
        JobKind::EnvPrepare => "env.prepare",
        JobKind::EnvInspectWorktree => "env.inspect",
        JobKind::SessionStart => "session.start",
        JobKind::SessionSendTurn => "session.turn",
        JobKind::SessionDeliverPermissionAnswer => "perm.deliver",
        JobKind::SessionInterrupt => "session.interrupt",
        JobKind::SessionStop => "session.stop",
        JobKind::ScmPushBranch => "scm.push",
        JobKind::ScmOpenChangeRequest => "scm.open_cr",
        JobKind::ScmPollChangeRequest => "scm.poll",
        JobKind::ScmMerge => "scm.merge",
        JobKind::EnvVerifyMerged => "sync.verify",
        JobKind::ScmDeleteBranch => "scm.delete_branch",
        JobKind::EnvTeardown => "env.teardown",
    }
}
