#![allow(
    clippy::unwrap_used,
    reason = "test helpers fail loudly on a broken setup"
)]

use saka_core::Ctx;
use saka_core::event::Event;
use saka_core::ids::{IdSource, ProjectId};
use saka_core::model::PermissionRule;
use saka_core::rejection::Rejection;
use saka_core::rule::{self, NewRule};
use saka_core::time::Timestamp;
use saka_core::vocab::{Effect, MatchKind};
use uuid::Uuid;

struct Counter(u128);

impl IdSource for Counter {
    fn next_id(&mut self) -> Uuid {
        self.0 += 1;
        Uuid::from_u128(self.0)
    }
}

const PROJECT: ProjectId = ProjectId(Uuid::from_u128(1));

fn bash(effect: Effect, match_kind: MatchKind, command: &str) -> NewRule {
    NewRule {
        effect,
        tool: "Bash".into(),
        match_kind,
        command: Some(command.into()),
        path: None,
        note: None,
    }
}

fn add(new: NewRule, active: &[PermissionRule]) -> Result<Event, Rejection> {
    let mut ids = Counter(100);
    let mut ctx = Ctx {
        now: Timestamp(5),
        ids: &mut ids,
    };
    rule::add(PROJECT, new, active, "human:operator:local", &mut ctx)
}

fn stored(event: &Event) -> PermissionRule {
    rule::evolve(None, event, Timestamp(5)).unwrap()
}

#[test]
fn a_hand_written_rule_has_no_run_and_no_request() {
    let event = add(
        NewRule {
            note: Some("tests are fine".into()),
            ..bash(Effect::Allow, MatchKind::Glob, "  go test *  ")
        },
        &[],
    )
    .unwrap();
    assert_eq!(event.owner(), None);
    let rule = stored(&event);
    assert_eq!(rule.effect, Effect::Allow);
    assert_eq!(rule.match_kind, MatchKind::Glob);
    assert_eq!(rule.command.as_deref(), Some("go test *"));
    assert_eq!(rule.note.as_deref(), Some("tests are fine"));
    assert_eq!(rule.permission_request_id, None);
    assert_eq!(rule.created_by, "human:operator:local");
}

#[test]
fn an_allow_glob_that_matches_every_command_is_too_broad_but_deny_and_ask_are_not() {
    for glob in ["*", "**", "***", " * ", "?*"] {
        assert_eq!(
            add(bash(Effect::Allow, MatchKind::Glob, glob), &[]).unwrap_err(),
            Rejection::RuleTooBroad,
            "{glob:?}"
        );
    }
    assert!(add(bash(Effect::Deny, MatchKind::Glob, "*"), &[]).is_ok());
    assert!(add(bash(Effect::Ask, MatchKind::Glob, "**"), &[]).is_ok());
    assert!(add(bash(Effect::Allow, MatchKind::Glob, "go *"), &[]).is_ok());
    assert!(add(bash(Effect::Allow, MatchKind::Exact, "*"), &[]).is_ok());
}

#[test]
fn a_broken_glob_a_bash_rule_without_a_command_and_a_command_on_a_file_tool_are_invalid() {
    let invalid = |field, reason| Rejection::RuleInvalid { field, reason };
    assert_eq!(
        add(bash(Effect::Deny, MatchKind::Glob, "[unclosed"), &[]).unwrap_err(),
        invalid("command", "glob")
    );
    let no_command = NewRule {
        command: None,
        ..bash(Effect::Deny, MatchKind::Exact, "x")
    };
    assert_eq!(
        add(no_command, &[]).unwrap_err(),
        invalid("command", "required")
    );
    assert_eq!(
        add(bash(Effect::Deny, MatchKind::Exact, "   "), &[]).unwrap_err(),
        invalid("command", "required")
    );
    let file_with_command = NewRule {
        tool: "Edit".into(),
        ..bash(Effect::Deny, MatchKind::Exact, "ls")
    };
    assert_eq!(
        add(file_with_command, &[]).unwrap_err(),
        invalid("command", "bash_only")
    );
    let bash_with_path = NewRule {
        path: Some("src/*".into()),
        ..bash(Effect::Deny, MatchKind::Exact, "ls")
    };
    assert_eq!(
        add(bash_with_path, &[]).unwrap_err(),
        invalid("path", "not_for_bash")
    );
    let bad_path = NewRule {
        tool: "Edit".into(),
        match_kind: MatchKind::Glob,
        command: None,
        path: Some("src/[".into()),
        ..bash(Effect::Deny, MatchKind::Glob, "x")
    };
    assert_eq!(add(bad_path, &[]).unwrap_err(), invalid("path", "glob"));
}

#[test]
fn field_limits_are_validation_failures() {
    let invalid = |field, code| Rejection::Invalid { field, code };
    let long = |n| "x".repeat(n);
    assert_eq!(
        add(bash(Effect::Deny, MatchKind::Exact, &long(2001)), &[]).unwrap_err(),
        invalid("command", "length")
    );
    assert!(add(bash(Effect::Deny, MatchKind::Exact, &long(2000)), &[]).is_ok());
    let note = |n| NewRule {
        note: Some(long(n)),
        ..bash(Effect::Deny, MatchKind::Exact, "ls")
    };
    assert_eq!(add(note(501), &[]).unwrap_err(), invalid("note", "length"));
    assert!(add(note(500), &[]).is_ok());
    let tool = |t: &str| NewRule {
        tool: t.into(),
        command: None,
        ..bash(Effect::Deny, MatchKind::Exact, "ls")
    };
    assert_eq!(
        add(tool("  "), &[]).unwrap_err(),
        invalid("tool", "required")
    );
    assert_eq!(
        add(tool(&long(101)), &[]).unwrap_err(),
        invalid("tool", "length")
    );
    assert_eq!(
        add(
            bash(Effect::Allow, MatchKind::Exact, "echo «secret:TOKEN»"),
            &[]
        )
        .unwrap_err(),
        invalid("command", "contains_secret")
    );
}

#[test]
fn an_identical_active_rule_is_a_duplicate_and_a_revoked_or_different_one_is_not() {
    let first = stored(&add(bash(Effect::Deny, MatchKind::Glob, "rm *"), &[]).unwrap());
    let again = add(
        bash(Effect::Deny, MatchKind::Glob, "rm *"),
        std::slice::from_ref(&first),
    );
    assert_eq!(
        again.unwrap_err(),
        Rejection::RuleDuplicate {
            rule_id: first.id.to_string()
        }
    );
    for different in [
        bash(Effect::Ask, MatchKind::Glob, "rm *"),
        bash(Effect::Deny, MatchKind::Exact, "rm *"),
        bash(Effect::Deny, MatchKind::Glob, "rm -rf *"),
    ] {
        assert!(add(different, std::slice::from_ref(&first)).is_ok());
    }
    let mut revoked = first.clone();
    revoked.revoked_at = Some(Timestamp(6));
    revoked.revoke_reason = Some("noisy".into());
    assert!(add(bash(Effect::Deny, MatchKind::Glob, "rm *"), &[revoked]).is_ok());
}
