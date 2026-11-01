use serde::{Deserialize, Serialize};

use crate::vocab::{CheckConclusion, CheckKind, CheckStatus, GateOutcome};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Check {
    pub name: String,
    pub kind: CheckKind,
    pub status: CheckStatus,
    pub conclusion: Option<CheckConclusion>,
    pub url: Option<String>,
}

impl Check {
    pub fn is_failure(&self) -> bool {
        self.status == CheckStatus::Completed
            && matches!(
                self.conclusion,
                Some(
                    CheckConclusion::Failure
                        | CheckConclusion::Cancelled
                        | CheckConclusion::TimedOut
                        | CheckConclusion::ActionRequired
                        | CheckConclusion::Stale
                        | CheckConclusion::Error
                )
            )
    }

    fn is_pass(&self) -> bool {
        self.status == CheckStatus::Completed
            && matches!(
                self.conclusion,
                Some(
                    CheckConclusion::Success | CheckConclusion::Neutral | CheckConclusion::Skipped
                )
            )
    }
}

pub fn outcome(checks: &[Check]) -> GateOutcome {
    if checks.iter().any(Check::is_failure) {
        GateOutcome::Failed
    } else if !checks.is_empty() && checks.iter().all(Check::is_pass) {
        GateOutcome::Passed
    } else {
        GateOutcome::Pending
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(status: CheckStatus, conclusion: Option<CheckConclusion>) -> Check {
        Check {
            name: "ci".into(),
            kind: CheckKind::CheckRun,
            status,
            conclusion,
            url: None,
        }
    }

    #[test]
    fn outcomes() {
        use CheckConclusion as C;
        use CheckStatus as S;
        assert_eq!(outcome(&[]), GateOutcome::Pending);
        assert_eq!(outcome(&[check(S::InProgress, None)]), GateOutcome::Pending);
        assert_eq!(
            outcome(&[
                check(S::Completed, Some(C::Success)),
                check(S::Completed, Some(C::Skipped))
            ]),
            GateOutcome::Passed
        );
        assert_eq!(
            outcome(&[
                check(S::Completed, Some(C::Success)),
                check(S::Queued, None)
            ]),
            GateOutcome::Pending
        );
        for bad in [
            C::Failure,
            C::Cancelled,
            C::TimedOut,
            C::ActionRequired,
            C::Stale,
            C::Error,
        ] {
            assert_eq!(
                outcome(&[check(S::InProgress, None), check(S::Completed, Some(bad))]),
                GateOutcome::Failed
            );
        }
    }
}
