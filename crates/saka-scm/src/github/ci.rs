//! CI at a head, normalized: check runs (the newest run per name) and commit statuses
//! become [`Check`]s with one status and conclusion vocabulary.

use std::collections::BTreeMap;

use saka_core::gate::Check;
use saka_core::vocab::{CheckConclusion, CheckKind, CheckStatus};
use serde_json::Value;

fn url(v: &Value) -> Option<String> {
    v.as_str().filter(|s| !s.is_empty()).map(String::from)
}

/// A conclusion this version does not know counts as a failure, so it never passes a gate.
fn conclusion(value: &str) -> CheckConclusion {
    value.parse().unwrap_or(CheckConclusion::Failure)
}

pub fn from_check_runs(runs: &[Value]) -> Vec<Check> {
    let mut newest: BTreeMap<String, (u64, Check)> = BTreeMap::new();
    for run in runs {
        let Some(name) = run["name"].as_str() else {
            continue;
        };
        let id = run["id"].as_u64().unwrap_or(0);
        let status = match run["status"].as_str() {
            Some("completed") => CheckStatus::Completed,
            Some("in_progress") => CheckStatus::InProgress,
            _ => CheckStatus::Queued,
        };
        let check = Check {
            name: name.to_string(),
            kind: CheckKind::CheckRun,
            status,
            conclusion: (status == CheckStatus::Completed)
                .then(|| conclusion(run["conclusion"].as_str().unwrap_or_default())),
            url: url(&run["html_url"]),
        };
        match newest.get(name) {
            Some((seen, _)) if *seen > id => {}
            _ => {
                newest.insert(name.to_string(), (id, check));
            }
        }
    }
    newest.into_values().map(|(_, check)| check).collect()
}

/// The combined status lists the latest status per context.
pub fn from_statuses(statuses: &[Value]) -> Vec<Check> {
    let mut by_context: BTreeMap<String, Check> = BTreeMap::new();
    for s in statuses {
        let Some(context) = s["context"].as_str() else {
            continue;
        };
        let state = s["state"].as_str().unwrap_or_default();
        let (status, concl) = match state {
            "pending" => (CheckStatus::InProgress, None),
            other => (CheckStatus::Completed, Some(conclusion(other))),
        };
        by_context.entry(context.to_string()).or_insert(Check {
            name: context.to_string(),
            kind: CheckKind::CommitStatus,
            status,
            conclusion: concl,
            url: url(&s["target_url"]),
        });
    }
    by_context.into_values().collect()
}

#[cfg(test)]
mod tests {
    use saka_core::gate::outcome;
    use saka_core::vocab::GateOutcome;
    use serde_json::json;

    use super::*;

    #[test]
    fn the_newest_run_per_name_wins_and_statuses_map_to_the_same_vocabulary() {
        let runs = [
            json!({ "id": 1, "name": "test", "status": "completed", "conclusion": "failure" }),
            json!({ "id": 3, "name": "test", "status": "completed", "conclusion": "success", "html_url": "https://x/3" }),
            json!({ "id": 2, "name": "lint", "status": "queued", "conclusion": null }),
            json!({ "id": 4, "name": "odd", "status": "completed", "conclusion": "startup_failure" }),
        ];
        let checks = from_check_runs(&runs);
        let summary: Vec<(&str, CheckStatus, Option<CheckConclusion>)> = checks
            .iter()
            .map(|c| (c.name.as_str(), c.status, c.conclusion))
            .collect();
        assert_eq!(
            summary,
            [
                ("lint", CheckStatus::Queued, None),
                (
                    "odd",
                    CheckStatus::Completed,
                    Some(CheckConclusion::Failure)
                ),
                (
                    "test",
                    CheckStatus::Completed,
                    Some(CheckConclusion::Success)
                ),
            ]
        );
        assert_eq!(checks[2].url.as_deref(), Some("https://x/3"));

        let statuses = [
            json!({ "context": "ci/a", "state": "pending" }),
            json!({ "context": "ci/b", "state": "error", "target_url": "" }),
            json!({ "context": "ci/c", "state": "success" }),
        ];
        let checks = from_statuses(&statuses);
        assert_eq!(checks[0].status, CheckStatus::InProgress);
        assert_eq!(checks[1].conclusion, Some(CheckConclusion::Error));
        assert_eq!(checks[1].url, None);
        assert_eq!(checks[2].conclusion, Some(CheckConclusion::Success));
        assert!(checks.iter().all(|c| c.kind == CheckKind::CommitStatus));
        assert_eq!(outcome(&checks), GateOutcome::Failed);
    }
}
