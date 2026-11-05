use std::collections::BTreeMap;

use saka_core::diagnostics::{AppRequirements, access_covers};
use serde::Serialize;

pub const REQUIRED_PERMISSIONS: &[(&str, &str)] = &[
    ("contents", "write"),
    ("pull_requests", "write"),
    ("checks", "read"),
    ("statuses", "read"),
    ("metadata", "read"),
];

pub const RECOMMENDED_PERMISSIONS: &[(&str, &str)] = &[("statuses", "write")];

pub const REQUIRED_EVENTS: &[&str] = &[
    "pull_request",
    "issue_comment",
    "check_run",
    "check_suite",
    "status",
];

pub const APP: AppRequirements = AppRequirements {
    permissions: REQUIRED_PERMISSIONS,
    recommended: RECOMMENDED_PERMISSIONS,
    events: REQUIRED_EVENTS,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MissingPermission {
    pub permission: String,
    pub requested: String,
    pub granted: Option<String>,
}

#[must_use]
pub fn manifest_permissions() -> BTreeMap<&'static str, &'static str> {
    let mut wanted = BTreeMap::new();
    for (permission, access) in REQUIRED_PERMISSIONS.iter().chain(RECOMMENDED_PERMISSIONS) {
        let entry = wanted.entry(*permission).or_insert(*access);
        if !access_covers(entry, access) {
            *entry = *access;
        }
    }
    wanted
}

#[must_use]
pub fn manifest_events() -> Vec<&'static str> {
    let mut events = REQUIRED_EVENTS.to_vec();
    events.sort_unstable();
    events
}

#[must_use]
pub fn missing_permissions(
    granted: &BTreeMap<String, String>,
    requested: &BTreeMap<&str, &str>,
) -> Vec<MissingPermission> {
    requested
        .iter()
        .filter_map(|(permission, access)| {
            let has = granted.get(*permission);
            (!has.is_some_and(|g| access_covers(g, access))).then(|| MissingPermission {
                permission: (*permission).to_string(),
                requested: (*access).to_string(),
                granted: has.cloned(),
            })
        })
        .collect()
}

#[must_use]
pub fn missing_events(granted: &[String], requested: &[&str]) -> Vec<String> {
    requested
        .iter()
        .filter(|e| !granted.iter().any(|have| have == *e))
        .map(|e| (*e).to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn granted(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn manifest_permissions_take_the_highest_level_of_required_and_recommended() {
        let expected: BTreeMap<&str, &str> = [
            ("checks", "read"),
            ("contents", "write"),
            ("metadata", "read"),
            ("pull_requests", "write"),
            ("statuses", "write"),
        ]
        .into_iter()
        .collect();
        assert_eq!(manifest_permissions(), expected);
    }

    #[test]
    fn manifest_events_are_the_required_events_sorted() {
        assert_eq!(
            manifest_events(),
            [
                "check_run",
                "check_suite",
                "issue_comment",
                "pull_request",
                "status"
            ]
        );
    }

    #[test]
    fn a_lower_level_is_missing_with_what_was_granted() {
        let have = granted(&[
            ("checks", "read"),
            ("contents", "write"),
            ("metadata", "read"),
            ("pull_requests", "read"),
            ("statuses", "write"),
        ]);
        assert_eq!(
            missing_permissions(&have, &manifest_permissions()),
            [MissingPermission {
                permission: "pull_requests".into(),
                requested: "write".into(),
                granted: Some("read".into()),
            }]
        );
    }

    #[test]
    fn an_absent_permission_is_missing_with_nothing_granted() {
        let have = granted(&[
            ("checks", "read"),
            ("contents", "write"),
            ("metadata", "read"),
            ("pull_requests", "write"),
        ]);
        assert_eq!(
            missing_permissions(&have, &manifest_permissions()),
            [MissingPermission {
                permission: "statuses".into(),
                requested: "write".into(),
                granted: None,
            }]
        );
    }

    #[test]
    fn admin_covers_write_and_read() {
        let have = granted(&[
            ("checks", "admin"),
            ("contents", "admin"),
            ("metadata", "admin"),
            ("pull_requests", "admin"),
            ("statuses", "admin"),
        ]);
        assert!(missing_permissions(&have, &manifest_permissions()).is_empty());
    }

    #[test]
    fn an_unknown_level_covers_nothing() {
        let have = granted(&[("metadata", "none")]);
        let wanted: BTreeMap<&str, &str> = [("metadata", "read")].into_iter().collect();
        assert_eq!(missing_permissions(&have, &wanted).len(), 1);
    }

    #[test]
    fn events_not_granted_are_missing_in_requested_order() {
        let have = vec!["pull_request".to_string(), "check_run".to_string()];
        assert_eq!(
            missing_events(&have, &manifest_events()),
            ["check_suite", "issue_comment", "status"]
        );
    }
}
