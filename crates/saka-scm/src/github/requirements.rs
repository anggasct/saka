use saka_core::diagnostics::AppRequirements;

pub const REQUIRED_PERMISSIONS: &[(&str, &str)] = &[
    ("contents", "write"),
    ("pull_requests", "write"),
    ("checks", "read"),
    ("statuses", "read"),
    ("metadata", "read"),
];

pub const RECOMMENDED_PERMISSIONS: &[(&str, &str)] = &[("statuses", "write")];

pub const REQUIRED_EVENTS: &[&str] = &["pull_request", "check_run", "check_suite", "status"];

pub const APP: AppRequirements = AppRequirements {
    permissions: REQUIRED_PERMISSIONS,
    recommended: RECOMMENDED_PERMISSIONS,
    events: REQUIRED_EVENTS,
};
