use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::vocab::{CheckConclusion, CheckKind, CheckStatus, GateMode, GateOutcome, GateSource};

pub const SAKA_STATUS_CONTEXT: &str = "saka/verify";
pub const DEFAULT_GATE_TIMEOUT_MINUTES: u32 = 30;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatePolicy {
    pub mode: GateMode,
    pub saka: bool,
    pub commands: Vec<String>,
    pub required_checks: Vec<String>,
    pub timeout_minutes: u32,
}

impl Default for GatePolicy {
    fn default() -> GatePolicy {
        GatePolicy {
            mode: GateMode::Auto,
            saka: true,
            commands: Vec::new(),
            required_checks: Vec::new(),
            timeout_minutes: DEFAULT_GATE_TIMEOUT_MINUTES,
        }
    }
}

impl GatePolicy {
    #[must_use]
    pub fn from_recipe(recipe: Option<&Value>) -> GatePolicy {
        let Some(gate) = recipe.and_then(|r| r.get("gate")).filter(|g| g.is_object()) else {
            return GatePolicy::default();
        };
        let strings = |key: &str| -> Vec<String> {
            gate[key]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        };
        GatePolicy {
            mode: gate["source"]
                .as_str()
                .and_then(|s| s.parse().ok())
                .unwrap_or(GateMode::Auto),
            saka: true,
            commands: strings("commands"),
            required_checks: strings("required_checks"),
            timeout_minutes: gate["timeout_minutes"]
                .as_u64()
                .and_then(|m| u32::try_from(m).ok())
                .unwrap_or(DEFAULT_GATE_TIMEOUT_MINUTES),
        }
    }

    #[must_use]
    pub fn without_saka(mut self) -> GatePolicy {
        self.saka = false;
        self
    }

    #[must_use]
    pub fn runs_saka(&self) -> bool {
        self.saka && self.mode != GateMode::Scm
    }

    #[must_use]
    pub fn scm_always_required(&self) -> bool {
        matches!(self.mode, GateMode::Scm | GateMode::Both) || !self.required_checks.is_empty()
    }

    #[must_use]
    pub fn uses(&self, source: GateSource) -> bool {
        match source {
            GateSource::Scm => self.mode != GateMode::Saka,
            GateSource::Saka => self.runs_saka(),
        }
    }

    #[must_use]
    pub fn required(&self, source: GateSource, checks: &[Check]) -> bool {
        match source {
            GateSource::Saka => self.runs_saka(),
            GateSource::Scm => {
                self.uses(GateSource::Scm)
                    && (self.scm_always_required() || !self.scm_checks(checks).is_empty())
            }
        }
    }

    #[must_use]
    pub fn scm_checks(&self, checks: &[Check]) -> Vec<Check> {
        checks
            .iter()
            .filter(|c| !(c.kind == CheckKind::CommitStatus && c.name == SAKA_STATUS_CONTEXT))
            .filter(|c| {
                self.required_checks.is_empty() || self.required_checks.iter().any(|r| r == &c.name)
            })
            .cloned()
            .collect()
    }

    #[must_use]
    pub fn missing_checks(&self, checks: &[Check]) -> Vec<String> {
        self.required_checks
            .iter()
            .filter(|r| !checks.iter().any(|c| &c.name == *r))
            .cloned()
            .collect()
    }

    #[must_use]
    pub fn scm_outcome(&self, checks: &[Check]) -> GateOutcome {
        let relevant = self.scm_checks(checks);
        match outcome(&relevant) {
            GateOutcome::Passed if !self.missing_checks(&relevant).is_empty() => {
                GateOutcome::Pending
            }
            other => other,
        }
    }
}

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

    fn named(name: &str, kind: CheckKind, conclusion: CheckConclusion) -> Check {
        Check {
            name: name.into(),
            kind,
            status: CheckStatus::Completed,
            conclusion: Some(conclusion),
            url: None,
        }
    }

    #[test]
    fn a_policy_reads_from_the_recipe_with_defaults() {
        assert_eq!(GatePolicy::from_recipe(None), GatePolicy::default());
        let recipe = serde_json::json!({ "gate": {
            "source": "both", "commands": ["make lint"], "required_checks": ["test"], "timeout_minutes": 5
        }});
        let p = GatePolicy::from_recipe(Some(&recipe));
        assert_eq!(p.mode, GateMode::Both);
        assert_eq!(p.commands, vec!["make lint"]);
        assert_eq!(p.required_checks, vec!["test"]);
        assert_eq!(p.timeout_minutes, 5);
        assert!(p.uses(GateSource::Scm) && p.uses(GateSource::Saka));
        let saka = GatePolicy {
            mode: GateMode::Saka,
            ..GatePolicy::default()
        };
        assert!(!saka.uses(GateSource::Scm));
        let scm = GatePolicy {
            mode: GateMode::Scm,
            ..GatePolicy::default()
        };
        assert!(!scm.runs_saka() && scm.scm_always_required());
        assert!(!GatePolicy::default().scm_always_required());
    }

    #[test]
    fn scm_outcomes_skip_our_own_status_and_honour_required_checks() {
        use CheckConclusion as C;
        let own = named(SAKA_STATUS_CONTEXT, CheckKind::CommitStatus, C::Failure);
        let lint = named("lint", CheckKind::CheckRun, C::Failure);
        let test = named("test", CheckKind::CheckRun, C::Success);
        let any = GatePolicy::default();
        assert_eq!(
            any.scm_outcome(&[own.clone(), test.clone()]),
            GateOutcome::Passed
        );
        assert_eq!(
            any.scm_outcome(std::slice::from_ref(&own)),
            GateOutcome::Pending
        );
        let required = GatePolicy {
            required_checks: vec!["test".into(), "build".into()],
            ..GatePolicy::default()
        };
        assert_eq!(
            required.scm_outcome(&[lint.clone(), test.clone()]),
            GateOutcome::Pending
        );
        assert_eq!(
            required.missing_checks(&[lint, test.clone()]),
            vec!["build"]
        );
        let build = named("build", CheckKind::CheckRun, C::Success);
        assert_eq!(required.scm_outcome(&[test, build]), GateOutcome::Passed);
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
