macro_rules! qualified {
    ($prefix:literal: $first:literal $(, $rest:literal)* $(,)?) => {
        concat!($prefix, $first $(, ", ", $prefix, $rest)*)
    };
}

macro_rules! spec_draft_columns {
    () => {
        spec_draft_columns!("")
    };
    ($prefix:literal) => {
        qualified!($prefix: "id", "project_id", "path", "slug", "idea", "locale", "mode", "context_paths", "state", "attempt", "current_attempt_id", "retries", "queued_at", "awaiting_since", "failure", "spec_id", "commit_sha", "version", "created_by", "created_at", "updated_at", "finished_at", "target_kind", "kind")
    };
}

macro_rules! spec_draft_attempt_columns {
    () => {
        spec_draft_attempt_columns!("")
    };
    ($prefix:literal) => {
        qualified!($prefix: "id", "spec_draft_id", "project_id", "attempt", "trigger", "reason", "note", "state", "base_sha", "budget", "budget_warnings", "resumed_lost", "rounds", "packet", "packet_sha256", "failure", "started_at", "ended_at")
    };
}

macro_rules! issue_intake_columns {
    () => {
        issue_intake_columns!("")
    };
    ($prefix:literal) => {
        qualified!($prefix: "id", "project_id", "repo_id", "number", "url", "title", "author", "state", "lane", "kind", "cause", "actor", "spec_id", "draft_id", "failure", "retries", "created_at", "updated_at")
    };
}

macro_rules! hook_run_columns {
    () => {
        hook_run_columns!("")
    };
    ($prefix:literal) => {
        qualified!($prefix: "id", "project_id", "hook", "action", "mode", "trigger", "state", "work_item_id", "spec_id", "repo_id", "sha", "context", "source_event_id", "rerun_of", "attempts", "result", "error", "queued_at", "finished_at", "duration_ms")
    };
}

macro_rules! deployment_columns {
    () => {
        deployment_columns!("")
    };
    ($prefix:literal) => {
        qualified!($prefix: "id", "project_id", "repo_id", "environment", "branch", "sha", "previous_sha", "cause", "state", "phase", "error", "action", "target_url", "verification", "actor", "reason", "queued_at", "started_at", "finished_at", "updated_at")
    };
}

macro_rules! deployment_check_columns {
    () => {
        deployment_check_columns!("")
    };
    ($prefix:literal) => {
        qualified!($prefix: "deployment_id", "phase", "ordinal", "command", "state", "exit_code", "http_status", "attempts", "duration_ms", "output_tail", "log", "started_at", "finished_at")
    };
}

macro_rules! inbox_item_columns {
    () => {
        inbox_item_columns!("")
    };
    ($prefix:literal) => {
        qualified!($prefix: "id", "project_id", "kind", "subject", "source_id", "work_item_id", "spec_id", "state", "params", "action", "opened_at", "answered_at", "closed_at")
    };
}

macro_rules! project_columns {
    () => {
        project_columns!("")
    };
    ($prefix:literal) => {
        qualified!($prefix: "id", "name", "workspace_repo_id", "project_path", "base_branch", "config_commit", "created_at", "updated_at", "config_sha256", "config_error", "workflow", "hook_skip", "verify")
    };
}

macro_rules! repo_columns {
    () => {
        repo_columns!("")
    };
    ($prefix:literal) => {
        qualified!($prefix: "id", "integration_id", "provider", "full_name", "url", "default_branch", "created_at", "updated_at")
    };
}

macro_rules! spec_columns {
    () => {
        spec_columns!("")
    };
    ($prefix:literal) => {
        qualified!($prefix: "id", "project_id", "path", "slug", "title", "state", "origin", "priority", "latest_revision_id", "approved_revision_id", "approved_at", "shipped_at", "created_at", "updated_at", "kind")
    };
}

macro_rules! session_columns {
    () => {
        session_columns!("")
    };
    ($prefix:literal) => {
        qualified!($prefix: "id", "project_id", "work_item_id", "stage_run_id", "spec_draft_id", "spec_draft_attempt_id", "environment_id", "cli", "cli_version", "cli_session_id", "model", "account", "state", "runner_unit", "socket_path", "log_path", "last_offset", "turns", "followups", "running_ms", "cost_usd_micros", "input_tokens", "output_tokens", "end_reason", "resumed_from_session_id", "resume_cause", "idle_since", "interrupt_requested", "started_at", "ended_at", "created_at", "updated_at")
    };
}

macro_rules! stage_run_columns {
    () => {
        stage_run_columns!("")
    };
    ($prefix:literal) => {
        qualified!($prefix: "id", "work_item_id", "project_id", "stage", "attempt", "state", "trigger", "reason", "entry_phase", "head_sha", "failure", "started_at", "ended_at", "created_at", "updated_at", "budget", "budget_warnings", "docs_head_sha", "queued_at", "queue_native", "fix", "branch_reset_from")
    };
}

macro_rules! environment_columns {
    () => {
        environment_columns!("")
    };
    ($prefix:literal) => {
        qualified!($prefix: "id", "project_id", "work_item_id", "spec_draft_id", "spec_draft_attempt_id", "repo_id", "root_path", "worktree_path", "branch", "base_sha", "state", "created_at", "updated_at", "recipe_json", "toolchains_json", "port_start", "port_count", "recipe_sha256", "path_scope_json", "docs_json")
    };
}

macro_rules! decision_request_columns {
    () => {
        decision_request_columns!("")
    };
    ($prefix:literal) => {
        qualified!($prefix: "id", "project_id", "session_id", "owner", "state", "question", "context", "options", "answer_option", "answer_note", "answered_by", "asked_at", "answered_at", "delivered_at", "expires_at")
    };
}

macro_rules! permission_request_columns {
    () => {
        permission_request_columns!("")
    };
    ($prefix:literal) => {
        qualified!($prefix: "id", "project_id", "work_item_id", "spec_draft_id", "session_id", "cli_request_id", "tool", "input", "summary", "state", "answer_source", "message", "requested_at", "answered_at", "delivered_at")
    };
}

macro_rules! session_activity_columns {
    () => {
        session_activity_columns!("")
    };
    ($prefix:literal) => {
        qualified!($prefix: "seq", "session_id", "offset", "work_item_id", "spec_draft_id", "project_id", "at", "kind", "summary", "data")
    };
}

macro_rules! blocker_columns {
    () => {
        blocker_columns!("")
    };
    ($prefix:literal) => {
        qualified!($prefix: "id", "work_item_id", "stage_run_id", "project_id", "code", "params", "state", "resolution", "opened_at", "resolved_at")
    };
}

macro_rules! change_request_columns {
    () => {
        change_request_columns!("")
    };
    ($prefix:literal) => {
        qualified!($prefix: "id", "project_id", "work_item_id", "role", "repo_id", "number", "url", "head_branch", "base_branch", "head_sha", "state", "merge_commit_sha", "merged_at", "skipped_at", "last_polled_at", "created_at", "updated_at", "update_head_sha")
    };
}

macro_rules! external_change_columns {
    () => {
        external_change_columns!("")
    };
    ($prefix:literal) => {
        qualified!($prefix: "work_item_id", "project_id", "title", "author", "from_fork", "draft", "spec_id", "step_id", "cause", "actor", "created_at")
    };
}

macro_rules! review_columns {
    () => {
        review_columns!("")
    };
    ($prefix:literal) => {
        qualified!($prefix: "id", "work_item_id", "stage_run_id", "session_id", "head_sha", "outcome", "basis", "summary", "findings", "questions", "posted_event", "posted_url", "posted_at", "created_at")
    };
}

macro_rules! regression_check_columns {
    () => {
        regression_check_columns!("")
    };
    ($prefix:literal) => {
        qualified!($prefix: "id", "work_item_id", "stage_run_id", "project_id", "attempt", "commit_sha", "base_sha", "outcome", "checks", "paths", "requested_at", "observed_at", "delivered_at")
    };
}

macro_rules! gate_result_columns {
    () => {
        gate_result_columns!("")
    };
    ($prefix:literal) => {
        qualified!($prefix: "id", "stage_run_id", "work_item_id", "project_id", "source", "head_sha", "outcome", "checks", "observed_at")
    };
}

macro_rules! usage_record_columns {
    () => {
        usage_record_columns!("")
    };
    ($prefix:literal) => {
        qualified!($prefix: "id", "project_id", "work_item_id", "stage_run_id", "spec_draft_id", "spec_draft_attempt_id", "session_id", "account", "turn", "status", "input_tokens", "output_tokens", "cost_usd_micros", "running_ms", "ended_at")
    };
}

macro_rules! permission_rule_columns {
    () => {
        permission_rule_columns!("")
    };
    ($prefix:literal) => {
        qualified!($prefix: "id", "project_id", "effect", "tool", "match_kind", "command", "path", "note", "permission_request_id", "created_by", "created_at", "revoked_at", "revoke_reason")
    };
}

macro_rules! account_quota_columns {
    () => {
        account_quota_columns!("")
    };
    ($prefix:literal) => {
        qualified!($prefix: "account", "window", "utilization", "status", "resets_at", "observed_at")
    };
}

macro_rules! spec_dependency_columns {
    () => {
        spec_dependency_columns!("")
    };
    ($prefix:literal) => {
        qualified!($prefix: "spec_id", "depends_on_spec_id", "depends_on_slug", "revision_id", "project_id", "created_at")
    };
}

macro_rules! work_item_dependency_columns {
    () => {
        work_item_dependency_columns!("")
    };
    ($prefix:literal) => {
        qualified!($prefix: "work_item_id", "depends_on_work_item_id", "project_id", "created_at")
    };
}

macro_rules! spec_revision_columns {
    () => {
        spec_revision_columns!("")
    };
    ($prefix:literal) => {
        qualified!($prefix: "id", "spec_id", "project_id", "source_commit", "content_hash", "context_hash", "content", "lint_ok", "lint_issues", "model", "parser_version", "created_at")
    };
}

macro_rules! secret_columns {
    () => {
        secret_columns!("")
    };
    ($prefix:literal) => {
        qualified!($prefix: "project_id", "name", "nonce", "ciphertext", "created_at", "updated_at")
    };
}

macro_rules! event_columns {
    () => {
        event_columns!("")
    };
    ($prefix:literal) => {
        qualified!($prefix: "seq", "id", "kind", "version", "stream_type", "stream_id", "project_id", "actor", "causation_id", "correlation_id", "occurred_at", "payload")
    };
}

macro_rules! action_request_columns {
    () => {
        action_request_columns!("")
    };
    ($prefix:literal) => {
        qualified!($prefix: "id", "actor", "idempotency_key", "request_hash", "action", "target_type", "target_id", "project_id", "reason", "response_status", "response_body", "created_at")
    };
}

macro_rules! user_link_columns {
    () => {
        user_link_columns!("")
    };
    ($prefix:literal) => {
        qualified!($prefix: "id", "owner", "integration_id", "provider_user_id", "login", "display_name", "email", "state", "relink_cause", "access_nonce", "access_ciphertext", "access_expires_at", "refresh_nonce", "refresh_ciphertext", "refresh_expires_at", "linked_at", "refreshed_at", "created_at", "updated_at")
    };
}

macro_rules! user_link_authorization_columns {
    () => {
        user_link_authorization_columns!("")
    };
    ($prefix:literal) => {
        qualified!($prefix: "id", "owner", "integration_id", "state", "device_nonce", "device_ciphertext", "user_code", "verification_uri", "interval_secs", "expires_at", "failure", "link_id", "created_at", "updated_at")
    };
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;
    use saka_core::time::Timestamp;

    fn migrated() -> Connection {
        let backups = tempfile::tempdir().unwrap();
        let mut conn = Connection::open_in_memory().unwrap();
        crate::migrate::migrate(&mut conn, backups.path(), Timestamp(0)).unwrap();
        conn
    }

    fn columns_of(conn: &Connection, table: &str) -> String {
        let mut stmt = conn
            .prepare("SELECT name FROM pragma_table_info(?1) ORDER BY cid")
            .unwrap();
        let names: Vec<String> = stmt
            .query_map([table], |row| row.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        names.join(", ")
    }

    #[test]
    fn every_list_names_all_the_columns_of_its_table_in_order() {
        let conn = migrated();
        for (table, listed) in [
            ("spec_draft", spec_draft_columns!()),
            ("spec_draft_attempt", spec_draft_attempt_columns!()),
            ("hook_run", hook_run_columns!()),
            ("deployment", deployment_columns!()),
            ("deployment_check", deployment_check_columns!()),
            ("inbox_item", inbox_item_columns!()),
            ("project", project_columns!()),
            ("repo", repo_columns!()),
            ("spec", spec_columns!()),
            ("session", session_columns!()),
            ("stage_run", stage_run_columns!()),
            ("environment", environment_columns!()),
            ("decision_request", decision_request_columns!()),
            ("permission_request", permission_request_columns!()),
            ("session_activity", session_activity_columns!()),
            ("blocker", blocker_columns!()),
            ("change_request", change_request_columns!()),
            ("external_change", external_change_columns!()),
            ("review", review_columns!()),
            ("regression_check", regression_check_columns!()),
            ("gate_result", gate_result_columns!()),
            ("usage_record", usage_record_columns!()),
            ("permission_rule", permission_rule_columns!()),
            ("account_quota", account_quota_columns!()),
            ("spec_dependency", spec_dependency_columns!()),
            ("work_item_dependency", work_item_dependency_columns!()),
            ("spec_revision", spec_revision_columns!()),
            ("secret", secret_columns!()),
            ("event", event_columns!()),
            ("action_request", action_request_columns!()),
            ("user_link", user_link_columns!()),
            (
                "user_link_authorization",
                user_link_authorization_columns!(),
            ),
        ] {
            assert_eq!(listed.replace('"', ""), columns_of(&conn, table), "{table}");
        }
    }
}
