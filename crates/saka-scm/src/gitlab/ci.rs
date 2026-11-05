use std::collections::BTreeMap;

use saka_core::gate::Check;
use saka_core::vocab::{CheckConclusion, CheckKind, CheckStatus};
use serde_json::Value;

fn url(v: &Value) -> Option<String> {
    v.as_str().filter(|s| !s.is_empty()).map(String::from)
}

fn status_of(state: &str, allow_failure: bool) -> (CheckStatus, Option<CheckConclusion>) {
    let done = |c| (CheckStatus::Completed, Some(c));
    match state {
        "running" | "canceling" => (CheckStatus::InProgress, None),
        "success" => done(CheckConclusion::Success),
        "failed" if allow_failure => done(CheckConclusion::Neutral),
        "failed" => done(CheckConclusion::Failure),
        "canceled" => done(CheckConclusion::Cancelled),
        "skipped" | "manual" => done(CheckConclusion::Skipped),
        _ => (CheckStatus::Queued, None),
    }
}

fn newest(items: &[Value], kind: CheckKind, link: &str) -> BTreeMap<String, Check> {
    let mut found: BTreeMap<String, (u64, Check)> = BTreeMap::new();
    for item in items {
        let Some(name) = item["name"].as_str() else {
            continue;
        };
        let id = item["id"].as_u64().unwrap_or(0);
        let (status, conclusion) = status_of(
            item["status"].as_str().unwrap_or_default(),
            item["allow_failure"].as_bool().unwrap_or(false),
        );
        let check = Check {
            name: name.to_string(),
            kind,
            status,
            conclusion,
            url: url(&item[link]),
        };
        match found.get(name) {
            Some((seen, _)) if *seen > id => {}
            _ => {
                found.insert(name.to_string(), (id, check));
            }
        }
    }
    found.into_iter().map(|(k, (_, c))| (k, c)).collect()
}

pub fn from_jobs(jobs: &[Value]) -> Vec<Check> {
    newest(jobs, CheckKind::CheckRun, "web_url")
        .into_values()
        .collect()
}

pub fn from_statuses(statuses: &[Value], jobs: &[Check]) -> Vec<Check> {
    newest(statuses, CheckKind::CommitStatus, "target_url")
        .into_iter()
        .filter(|(name, _)| !jobs.iter().any(|j| &j.name == name))
        .map(|(_, check)| check)
        .collect()
}

#[cfg(test)]
mod tests {
    use saka_core::gate::outcome;
    use saka_core::vocab::GateOutcome;
    use serde_json::json;

    use super::*;

    fn summary(checks: &[Check]) -> Vec<(&str, CheckStatus, Option<CheckConclusion>)> {
        checks
            .iter()
            .map(|c| (c.name.as_str(), c.status, c.conclusion))
            .collect()
    }

    #[test]
    fn every_gitlab_status_maps_to_one_check_state() {
        let cases = [
            ("created", CheckStatus::Queued, None),
            ("pending", CheckStatus::Queued, None),
            ("waiting_for_resource", CheckStatus::Queued, None),
            ("waiting_for_callback", CheckStatus::Queued, None),
            ("preparing", CheckStatus::Queued, None),
            ("scheduled", CheckStatus::Queued, None),
            ("something_new", CheckStatus::Queued, None),
            ("running", CheckStatus::InProgress, None),
            ("canceling", CheckStatus::InProgress, None),
            (
                "success",
                CheckStatus::Completed,
                Some(CheckConclusion::Success),
            ),
            (
                "failed",
                CheckStatus::Completed,
                Some(CheckConclusion::Failure),
            ),
            (
                "canceled",
                CheckStatus::Completed,
                Some(CheckConclusion::Cancelled),
            ),
            (
                "skipped",
                CheckStatus::Completed,
                Some(CheckConclusion::Skipped),
            ),
            (
                "manual",
                CheckStatus::Completed,
                Some(CheckConclusion::Skipped),
            ),
        ];
        for (state, status, conclusion) in cases {
            assert_eq!(status_of(state, false), (status, conclusion), "{state}");
        }
        assert_eq!(
            status_of("failed", true),
            (CheckStatus::Completed, Some(CheckConclusion::Neutral))
        );
    }

    #[test]
    fn the_newest_job_per_name_wins_and_a_failure_allowed_job_does_not_fail_the_gate() {
        let jobs = [
            json!({ "id": 1, "name": "test", "status": "failed" }),
            json!({ "id": 3, "name": "test", "status": "success", "web_url": "https://x/3" }),
            json!({ "id": 2, "name": "lint", "status": "created" }),
            json!({ "id": 4, "name": "flaky", "status": "failed", "allow_failure": true }),
            json!({ "id": 5, "status": "success" }),
        ];
        let checks = from_jobs(&jobs);
        assert_eq!(
            summary(&checks),
            [
                (
                    "flaky",
                    CheckStatus::Completed,
                    Some(CheckConclusion::Neutral)
                ),
                ("lint", CheckStatus::Queued, None),
                (
                    "test",
                    CheckStatus::Completed,
                    Some(CheckConclusion::Success)
                ),
            ]
        );
        assert_eq!(checks[2].url.as_deref(), Some("https://x/3"));
        assert!(checks.iter().all(|c| c.kind == CheckKind::CheckRun));
        assert_eq!(outcome(&checks[..1]), GateOutcome::Passed);
    }

    #[test]
    fn external_statuses_skip_names_already_reported_as_jobs() {
        let jobs = from_jobs(&[json!({ "id": 1, "name": "test", "status": "success" })]);
        let statuses = [
            json!({ "id": 9, "name": "test", "status": "success" }),
            json!({ "id": 10, "name": "deploy/preview", "status": "failed", "target_url": "" }),
            json!({ "id": 8, "name": "deploy/preview", "status": "success" }),
            json!({ "id": 11, "name": "saka/verify", "status": "running", "target_url": "https://ci/1" }),
        ];
        let found = from_statuses(&statuses, &jobs);
        assert_eq!(
            summary(&found),
            [
                (
                    "deploy/preview",
                    CheckStatus::Completed,
                    Some(CheckConclusion::Failure)
                ),
                ("saka/verify", CheckStatus::InProgress, None),
            ]
        );
        assert_eq!(found[0].url, None);
        assert_eq!(found[1].url.as_deref(), Some("https://ci/1"));
        assert!(found.iter().all(|c| c.kind == CheckKind::CommitStatus));
    }
}
