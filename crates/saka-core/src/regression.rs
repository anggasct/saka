use serde_json::{Value, json};

use crate::model::{RegressionCheck, RegressionRun};
use crate::scope::PathScope;
use crate::vocab::RegressionOutcome;

pub const MAX_ATTEMPTS: u32 = 3;
pub const JOB_BUDGET_MS: u64 = 30 * 60_000;
pub const COMMIT_CHECK: &str = "saka/regression-commit";

pub const DEFAULT_TEST_PATHS: [&str; 12] = [
    "**/tests/**",
    "**/test/**",
    "**/__tests__/**",
    "**/spec/**",
    "**/*_test.*",
    "**/*.test.*",
    "**/*.spec.*",
    "**/test_*.py",
    "**/*_spec.rb",
    "**/*Test.java",
    "**/*Tests.cs",
    "**/*Test.kt",
];

pub fn test_patterns(extra: &[String]) -> Vec<String> {
    DEFAULT_TEST_PATHS
        .iter()
        .map(|p| (*p).to_string())
        .chain(extra.iter().cloned())
        .collect()
}

pub fn non_test_paths(extra: &[String], changed: &[String], truncated: bool) -> Vec<String> {
    let Ok(scope) = PathScope::parse(&test_patterns(extra)) else {
        return changed.to_vec();
    };
    match scope.violation(changed, truncated) {
        Some(violation) => violation.paths,
        None => Vec::new(),
    }
}

pub fn outcome_of(runs: &[RegressionRun]) -> RegressionOutcome {
    if runs.is_empty() || runs.iter().any(|r| r.timed_out) {
        RegressionOutcome::Error
    } else if runs.iter().any(RegressionRun::failed) {
        RegressionOutcome::Reproduced
    } else {
        RegressionOutcome::NotReproduced
    }
}

pub fn last_outcome(checks: &[&RegressionCheck]) -> Value {
    match checks.last().and_then(|c| c.outcome) {
        Some(outcome) => json!(outcome),
        None => json!("none"),
    }
}

const TAIL_CHARS: usize = 1500;

fn short(sha: &str) -> String {
    sha.chars().take(12).collect()
}

fn tail(text: &str) -> String {
    let chars: Vec<char> = text.trim_end().chars().collect();
    let start = chars.len().saturating_sub(TAIL_CHARS);
    chars[start..].iter().collect()
}

pub fn turn_text(check: &RegressionCheck) -> String {
    let commit = short(&check.commit_sha);
    let left = MAX_ATTEMPTS.saturating_sub(check.attempt);
    let again = format!(
        "Change only test files, commit, and call `regression_check` again, then end your turn. You have {left} check(s) left."
    );
    let mut text = format!(
        "Saka checked your regression test at commit {commit} (check {} of {MAX_ATTEMPTS}).\n\n",
        check.attempt
    );
    match check.outcome {
        Some(RegressionOutcome::Reproduced) => {
            text.push_str(
                "Result: reproduced. The regression commands fail at that commit, so the test catches the bug.\n",
            );
            for run in check.checks.iter().filter(|r| r.failed()) {
                text.push_str(&format!("\n`{}` failed", run.command));
                if let Some(code) = run.exit_code {
                    text.push_str(&format!(" with exit code {code}"));
                }
                text.push_str(":\n```\n");
                text.push_str(&tail(&run.output_tail));
                text.push_str("\n```\n");
            }
            text.push_str(
                "\nNow fix the bug with the smallest change that makes them pass. Keep the regression test as committed. Commit the fix with a Conventional Commits message and do not push.",
            );
        }
        Some(RegressionOutcome::NotReproduced) => {
            text.push_str(
                "Result: not reproduced. Every regression command passed at that commit, so the test does not catch the bug.\n\nWrite a test that fails on the current code. ",
            );
            text.push_str(&again);
        }
        Some(RegressionOutcome::InvalidPaths) => {
            text.push_str(
                "Result: invalid paths. The commit changes files that are not tests, and the test-first commit may change test files only:\n",
            );
            for path in &check.paths {
                text.push_str(&format!("- {path}\n"));
            }
            text.push_str("\nMove those changes out of this commit. ");
            text.push_str(&again);
        }
        Some(RegressionOutcome::Error) | None => {
            text.push_str(
                "Result: error. Saka could not finish running the regression commands (a timeout, or the worktree was not clean at that commit).\n\nKeep the worktree clean and the test as committed. Call `regression_check` again, then end your turn.",
            );
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(code: Option<i32>, timed_out: bool) -> RegressionRun {
        RegressionRun {
            command: "make test".into(),
            exit_code: code,
            timed_out,
            duration_ms: 1,
            output_tail: String::new(),
        }
    }

    fn paths(items: &[&str]) -> Vec<String> {
        items.iter().map(|p| (*p).to_string()).collect()
    }

    #[test]
    fn a_failing_command_reproduces_the_bug_and_a_timeout_is_an_error() {
        assert_eq!(
            outcome_of(&[run(Some(0), false), run(Some(1), false)]),
            RegressionOutcome::Reproduced
        );
        assert_eq!(
            outcome_of(&[run(Some(0), false)]),
            RegressionOutcome::NotReproduced
        );
        assert_eq!(
            outcome_of(&[run(Some(1), false), run(None, true)]),
            RegressionOutcome::Error
        );
        assert_eq!(outcome_of(&[]), RegressionOutcome::Error);
    }

    #[test]
    fn only_paths_outside_the_default_and_configured_test_patterns_are_refused() {
        let changed = paths(&[
            "tests/login.rs",
            "web/src/login.test.ts",
            "pkg/user_test.go",
            "src/fixtures/seed.json",
            "src/lib.rs",
        ]);
        assert_eq!(
            non_test_paths(&[], &changed, false),
            paths(&["src/fixtures/seed.json", "src/lib.rs"])
        );
        assert_eq!(
            non_test_paths(&paths(&["src/**/fixtures/**"]), &changed, false),
            paths(&["src/lib.rs"])
        );
        assert!(non_test_paths(&[], &paths(&["tests/a.rs"]), false).is_empty());
    }

    #[test]
    fn the_turn_text_names_the_next_step_for_every_outcome() {
        let check = |outcome, paths: &[&str]| RegressionCheck {
            id: crate::ids::RegressionCheckId(uuid::Uuid::nil()),
            work_item_id: crate::ids::WorkItemId(uuid::Uuid::nil()),
            stage_run_id: crate::ids::StageRunId(uuid::Uuid::nil()),
            attempt: 2,
            commit_sha: "a".repeat(40),
            base_sha: "b".repeat(40),
            outcome: Some(outcome),
            checks: vec![RegressionRun {
                output_tail: "boom".into(),
                ..run(Some(101), false)
            }],
            paths: paths.iter().map(|p| (*p).to_string()).collect(),
            requested_at: crate::time::Timestamp(0),
            observed_at: Some(crate::time::Timestamp(1)),
            delivered_at: None,
        };
        let reproduced = turn_text(&check(RegressionOutcome::Reproduced, &[]));
        assert!(reproduced.contains("check 2 of 3") && reproduced.contains("exit code 101"));
        assert!(reproduced.contains("boom") && reproduced.contains("Now fix the bug"));
        let invalid = turn_text(&check(RegressionOutcome::InvalidPaths, &["src/lib.rs"]));
        assert!(invalid.contains("- src/lib.rs") && invalid.contains("1 check(s) left"));
        assert!(
            turn_text(&check(RegressionOutcome::NotReproduced, &[])).contains("does not catch")
        );
        assert!(
            turn_text(&check(RegressionOutcome::Error, &[]))
                .contains("Call `regression_check` again")
        );
    }
}
