use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown {vocabulary} value: {value}")]
pub struct UnknownValue {
    pub vocabulary: &'static str,
    pub value: String,
}

macro_rules! vocab {
    ($(#[$doc:meta])* $name:ident { $($variant:ident => $wire:literal),* $(,)? }) => {
        $(#[$doc])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        pub enum $name {
            $(#[serde(rename = $wire)] $variant),*
        }

        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$variant),*];

            pub fn as_str(self) -> &'static str {
                match self { $($name::$variant => $wire),* }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl FromStr for $name {
            type Err = UnknownValue;
            fn from_str(s: &str) -> Result<$name, UnknownValue> {
                match s {
                    $($wire => Ok($name::$variant),)*
                    _ => Err(UnknownValue { vocabulary: stringify!($name), value: s.to_string() }),
                }
            }
        }
    };
}

vocab!(SpecState { Draft => "draft", Ready => "ready", Approved => "approved", Shipped => "shipped", Withdrawn => "withdrawn" });
vocab!(SpecOrigin { Manual => "manual", Architect => "architect", Intake => "intake" });
vocab!(Autonomy { Strict => "strict", Standard => "standard", Autonomous => "autonomous" });
vocab!(WorkflowPreset { FullLoop => "full_loop", ReviewOnly => "review_only", BuildOnly => "build_only", Custom => "custom" });
vocab!(WorkItemOrigin { SpecStep => "spec_step", ExternalChange => "external_change" });
vocab!(WorkItemState {
    Queued => "queued",
    AwaitingStart => "awaiting_start",
    Admitted => "admitted",
    InStage => "in_stage",
    Blocked => "blocked",
    TakenOver => "taken_over",
    Done => "done",
    Cancelled => "cancelled",
    Superseded => "superseded",
});
vocab!(SupersedeReason { StepRemoved => "step_removed", SpecWithdrawn => "spec_withdrawn" });
vocab!(PlanAction { Keep => "keep", Repin => "repin", Restore => "restore", Add => "add", Supersede => "supersede", Conflict => "conflict" });
vocab!(ConflictReason { Changed => "changed", Removed => "removed" });
vocab!(StepField { Work => "work", Ac => "ac", Commands => "commands", After => "after" });
vocab!(Stage { Dev => "dev", Review => "review", Fix => "fix", Merge => "merge", Sync => "sync" });
vocab!(
    StagePhase {
        PreparingEnvironment => "preparing_environment",
        RunningSession => "running_session",
        Pushing => "pushing",
        OpeningChange => "opening_change",
        AwaitingCi => "awaiting_ci",
        AwaitingMerge => "awaiting_merge",
        Merging => "merging",
        Verifying => "verifying",
        TearingDown => "tearing_down",
    }
);
vocab!(StageRunState {
    Pending => "pending",
    Running => "running",
    Succeeded => "succeeded",
    Failed => "failed",
    Blocked => "blocked",
    Cancelled => "cancelled",
});
vocab!(Trigger { Auto => "auto", Manual => "manual" });
vocab!(ReloadTrigger { Poll => "poll", Operator => "operator" });
vocab!(ReloadOutcome { Changed => "changed", Unchanged => "unchanged" });
vocab!(
    BlockerCode {
        EnvSetupFailed => "env_setup_failed",
        EnvRecipeInvalid => "env_recipe_invalid",
        ToolchainUnpinned => "toolchain_unpinned",
        ToolchainUnavailable => "toolchain_unavailable",
        SecretMissing => "secret_missing",
        SecretUnreadable => "secret_unreadable",
        SessionFailed => "session_failed",
        SessionLost => "session_lost",
        SessionTimeout => "session_timeout",
        PermissionExpired => "permission_expired",
        QuotaExhausted => "quota_exhausted",
        NoChanges => "no_changes",
        CiRed => "ci_red",
        VerdictFailed => "verdict_failed",
        ReviewRoundsExhausted => "review_rounds_exhausted",
        MergeConflict => "merge_conflict",
        PushRejected => "push_rejected",
        ChangeClosed => "change_closed",
        ScmUnavailable => "scm_unavailable",
        SpecUnclear => "spec_unclear",
        DodFailed => "dod_failed",
        HookFailed => "hook_failed",
        BudgetExhausted => "budget_exhausted",
    }
);
vocab!(Tool { Go => "go", Node => "node", Pnpm => "pnpm" });
vocab!(SecretPurpose { Setup => "setup", Session => "session" });
vocab!(ContextOrigin { Link => "link", Always => "always" });
vocab!(BlockerResolution { Retried => "retried", Cancelled => "cancelled", Auto => "auto" });
vocab!(EnvironmentState { Preparing => "preparing", Ready => "ready", Failed => "failed", TeardownFailed => "teardown_failed", TornDown => "torn_down" });
vocab!(SessionState {
    Starting => "starting",
    Running => "running",
    Waiting => "waiting",
    Idle => "idle",
    Interrupted => "interrupted",
    Lost => "lost",
    Resumed => "resumed",
    Ended => "ended",
    Failed => "failed",
});
vocab!(ResumeCause { Lost => "lost", Retry => "retry" });
vocab!(InterruptBy { Operator => "operator", Saka => "saka" });
vocab!(
    ResumeReason {
        RunnerLost => "runner_lost",
        OperatorStopped => "operator_stopped",
        IdleTimeout => "idle_timeout",
        PermissionExpired => "permission_expired",
        BudgetExhausted => "budget_exhausted",
        QuotaExhausted => "quota_exhausted",
        Retried => "retried",
    }
);
vocab!(BudgetDimension { RunningMinutes => "running_minutes", Turns => "turns", Tokens => "tokens", CostUsd => "cost_usd" });
vocab!(PermissionState { Pending => "pending", Allowed => "allowed", Denied => "denied", Expired => "expired" });
vocab!(ChangeState { Open => "open", Merged => "merged", Closed => "closed" });
vocab!(GateSource { Scm => "scm", Saka => "saka" });
vocab!(GateOutcome { Pending => "pending", Passed => "passed", Failed => "failed" });
vocab!(CheckKind { CheckRun => "check_run", CommitStatus => "commit_status", Command => "command" });
vocab!(GateMode { Auto => "auto", Scm => "scm", Saka => "saka", Both => "both" });
vocab!(CheckStatus { Queued => "queued", InProgress => "in_progress", Completed => "completed" });
vocab!(CheckConclusion {
    Success => "success",
    Failure => "failure",
    Neutral => "neutral",
    Skipped => "skipped",
    Cancelled => "cancelled",
    TimedOut => "timed_out",
    ActionRequired => "action_required",
    Stale => "stale",
    Error => "error",
});
vocab!(MergeMethod { Merge => "merge", Squash => "squash", Rebase => "rebase" });
vocab!(SpecDraftState {
    Queued => "queued",
    Preparing => "preparing",
    Drafting => "drafting",
    Committing => "committing",
    Succeeded => "succeeded",
    Failed => "failed",
    Cancelled => "cancelled",
});
vocab!(DraftAttemptState { Running => "running", Succeeded => "succeeded", Failed => "failed", Cancelled => "cancelled" });
vocab!(DraftLocale { En => "en", Id => "id" });
vocab!(LintOutcome { Passed => "passed", Failed => "failed", Missing => "missing" });
vocab!(PacketOrigin { Template => "template", Rules => "rules", Selected => "selected", Always => "always" });
vocab!(PrimaryAction { Retry => "retry", OpenWritten => "open_written", OpenFile => "open_file" });
vocab!(TargetTaken { FileExists => "file_exists", SpecRegistered => "spec_registered", SlugTaken => "slug_taken", DraftActive => "draft_active" });
vocab!(
    DraftFailureCode {
        LintRoundsExhausted => "lint_rounds_exhausted",
        BudgetExhausted => "budget_exhausted",
        QuotaExhausted => "quota_exhausted",
        SessionFailed => "session_failed",
        SessionLost => "session_lost",
        SessionTimeout => "session_timeout",
        PermissionExpired => "permission_expired",
        EnvSetupFailed => "env_setup_failed",
        ContextUnavailable => "context_unavailable",
        WorktreeUnreadable => "worktree_unreadable",
        ScmUnavailable => "scm_unavailable",
        PushRejected => "push_rejected",
        TargetExists => "target_exists",
        TargetPathConflict => "target_path_conflict",
        RegisterFailed => "register_failed",
    }
);

impl SessionState {
    pub fn is_live(self) -> bool {
        matches!(
            self,
            SessionState::Starting
                | SessionState::Running
                | SessionState::Waiting
                | SessionState::Idle
                | SessionState::Interrupted
        )
    }
}

impl SpecDraftState {
    pub fn is_terminal(self) -> bool {
        matches!(self, SpecDraftState::Succeeded | SpecDraftState::Cancelled)
    }

    pub fn is_finished(self) -> bool {
        matches!(
            self,
            SpecDraftState::Succeeded | SpecDraftState::Failed | SpecDraftState::Cancelled
        )
    }

    pub fn holds_slot(self) -> bool {
        matches!(
            self,
            SpecDraftState::Preparing | SpecDraftState::Drafting | SpecDraftState::Committing
        )
    }

    pub fn holds_path(self) -> bool {
        !self.is_terminal()
    }
}

impl DraftFailureCode {
    pub fn is_retryable(self) -> bool {
        !matches!(
            self,
            DraftFailureCode::TargetExists
                | DraftFailureCode::TargetPathConflict
                | DraftFailureCode::RegisterFailed
        )
    }

    pub fn from_blocker(code: BlockerCode) -> Option<DraftFailureCode> {
        Some(match code {
            BlockerCode::BudgetExhausted => DraftFailureCode::BudgetExhausted,
            BlockerCode::QuotaExhausted => DraftFailureCode::QuotaExhausted,
            BlockerCode::SessionFailed => DraftFailureCode::SessionFailed,
            BlockerCode::SessionLost => DraftFailureCode::SessionLost,
            BlockerCode::SessionTimeout => DraftFailureCode::SessionTimeout,
            BlockerCode::PermissionExpired => DraftFailureCode::PermissionExpired,
            BlockerCode::EnvSetupFailed => DraftFailureCode::EnvSetupFailed,
            BlockerCode::ScmUnavailable => DraftFailureCode::ScmUnavailable,
            BlockerCode::PushRejected => DraftFailureCode::PushRejected,
            _ => return None,
        })
    }
}

impl WorkItemState {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            WorkItemState::Done | WorkItemState::Cancelled | WorkItemState::Superseded
        )
    }

    pub fn has_started(self) -> bool {
        !matches!(
            self,
            WorkItemState::Queued | WorkItemState::AwaitingStart | WorkItemState::Superseded
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_names_round_trip() {
        for code in BlockerCode::ALL {
            assert_eq!(code.as_str().parse::<BlockerCode>(), Ok(*code));
            assert_eq!(
                serde_json::to_value(code).unwrap(),
                serde_json::json!(code.as_str())
            );
        }
        for phase in StagePhase::ALL {
            assert_eq!(phase.as_str().parse::<StagePhase>(), Ok(*phase));
        }
        assert!("nope".parse::<WorkItemState>().is_err());
    }
}
