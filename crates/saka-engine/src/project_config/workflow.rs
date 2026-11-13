use std::collections::BTreeMap;

use saka_core::scope::{PathScope, ScopeIssue};
use saka_core::vocab::{IssueTrigger, Stage, Trigger, WorkKind, WorkflowPreset};
use saka_core::workflow::{
    ChangeEntry, IssueBugfix, IssueEntry, KindIssue, KindOverride, KindRefusal, MAX_CI_RERUNS,
    MAX_FILTER_ENTRIES, MAX_FIX_ROUNDS, MAX_ISSUE_COMMANDS, MAX_ISSUE_LABEL_CHARS,
    MAX_ISSUE_LABELS, MAX_ISSUE_MAX_OPEN, MAX_TEST_PATHS, Workflow, WorkflowPolicy, filter_text_ok,
};
use saka_spec::{QuickSpec, check_quick};
use toml::Spanned;
use toml::de::{DeTable, DeValue};

use super::{Collector, autonomy, get, line_of};

pub type KindLines = BTreeMap<(WorkKind, Stage), usize>;

const KEYS: &[&str] = &[
    "preset",
    "spec",
    "change_request",
    "issue",
    "stages",
    "fix_rounds",
    "ci_reruns",
    "kinds",
];
const ISSUE_KEYS: &[&str] = &[
    "trigger",
    "trigger_label",
    "labels",
    "exclude_labels",
    "kind_labels",
    "default_kind",
    "include_confidential",
    "max_open",
    "bugfix",
];
const ISSUE_BUGFIX_KEYS: &[&str] = &["done_when", "regression"];
const KIND_KEYS: &[&str] = &["stages", "autonomy"];
const BUGFIX_KEYS: &[&str] = &["stages", "autonomy", "test_paths"];
const ENTRY_KEYS: &[&str] = &[
    "trigger",
    "labels",
    "authors",
    "branches",
    "paths",
    "include_forks",
    "include_drafts",
    "trigger_label",
];
const STAGE_KEYS: &[(&str, Stage)] = &[
    ("dev", Stage::Dev),
    ("review", Stage::Review),
    ("fix", Stage::Fix),
    ("merge", Stage::Merge),
];

fn trigger(c: &mut Collector<'_>, value: &Spanned<DeValue<'_>>, field: &str) -> Option<Trigger> {
    let parsed = value
        .get_ref()
        .as_str()
        .and_then(|s| s.parse::<Trigger>().ok());
    if parsed.is_none() {
        c.push("field_invalid", field, Some(value.span().start));
    }
    parsed
}

fn flag(c: &mut Collector<'_>, value: &Spanned<DeValue<'_>>, field: &str) -> Option<bool> {
    let parsed = value.get_ref().as_bool();
    if parsed.is_none() {
        c.push("field_invalid", field, Some(value.span().start));
    }
    parsed
}

fn list(c: &mut Collector<'_>, value: &Spanned<DeValue<'_>>, field: &str) -> Option<Vec<String>> {
    let Some(items) = value.get_ref().as_array() else {
        c.push("field_invalid", field, Some(value.span().start));
        return None;
    };
    if items.len() > MAX_FILTER_ENTRIES {
        c.push("field_invalid", field, Some(value.span().start));
        return None;
    }
    let mut out = Vec::with_capacity(items.len());
    let mut valid = true;
    for (j, item) in items.iter().enumerate() {
        match item.get_ref().as_str().filter(|s| filter_text_ok(s)) {
            Some(text) => out.push(text.to_string()),
            None => {
                c.push(
                    "field_invalid",
                    &format!("{field}[{j}]"),
                    Some(item.span().start),
                );
                valid = false;
            }
        }
    }
    valid.then_some(out)
}

fn patterns(
    c: &mut Collector<'_>,
    value: &Spanned<DeValue<'_>>,
    field: &str,
) -> Option<Vec<String>> {
    let items = list(c, value, field)?;
    match PathScope::parse(&items) {
        Ok(_) => Some(items),
        Err(issues) => {
            let code = if issues.contains(&ScopeIssue::NoInclude) {
                "path_scope_empty"
            } else {
                "field_invalid"
            };
            c.push(code, field, Some(value.span().start));
            None
        }
    }
}

fn entry(c: &mut Collector<'_>, table: &DeTable<'_>) -> ChangeEntry {
    let mut entry = ChangeEntry::default();
    c.unknown(table, "workflow.change_request.", ENTRY_KEYS);
    let field = |key: &str| format!("workflow.change_request.{key}");
    if let Some(v) = get(table, "trigger")
        && let Some(t) = trigger(c, v, &field("trigger"))
    {
        entry.trigger = t;
    }
    for (key, target) in [
        ("labels", &mut entry.labels),
        ("authors", &mut entry.authors),
    ] {
        if let Some(v) = get(table, key)
            && let Some(items) = list(c, v, &field(key))
        {
            *target = items;
        }
    }
    for (key, target) in [
        ("branches", &mut entry.branches),
        ("paths", &mut entry.paths),
    ] {
        if let Some(v) = get(table, key)
            && let Some(items) = patterns(c, v, &field(key))
        {
            *target = items;
        }
    }
    for (key, target) in [
        ("include_forks", &mut entry.include_forks),
        ("include_drafts", &mut entry.include_drafts),
    ] {
        if let Some(v) = get(table, key)
            && let Some(b) = flag(c, v, &field(key))
        {
            *target = b;
        }
    }
    if let Some(v) = get(table, "trigger_label") {
        match v.get_ref().as_str().filter(|s| filter_text_ok(s)) {
            Some(label) => entry.trigger_label = label.to_string(),
            None => c.push(
                "field_invalid",
                &field("trigger_label"),
                Some(v.span().start),
            ),
        }
    }
    entry
}

fn issue_label(text: &str) -> bool {
    let chars = text.chars().count();
    (1..=MAX_ISSUE_LABEL_CHARS).contains(&chars)
        && !text.contains(',')
        && !text.chars().any(char::is_control)
}

fn issue_labels(
    c: &mut Collector<'_>,
    value: &Spanned<DeValue<'_>>,
    field: &str,
) -> Option<Vec<String>> {
    let Some(items) = value.get_ref().as_array() else {
        c.push("field_invalid", field, Some(value.span().start));
        return None;
    };
    if items.len() > MAX_ISSUE_LABELS {
        c.push("field_invalid", field, Some(value.span().start));
        return None;
    }
    let mut out = Vec::with_capacity(items.len());
    let mut valid = true;
    for (j, item) in items.iter().enumerate() {
        match item.get_ref().as_str().filter(|s| issue_label(s)) {
            Some(text) => out.push(text.to_string()),
            None => {
                c.push(
                    "field_invalid",
                    &format!("{field}[{j}]"),
                    Some(item.span().start),
                );
                valid = false;
            }
        }
    }
    valid.then_some(out)
}

fn issue_kind(
    c: &mut Collector<'_>,
    value: &Spanned<DeValue<'_>>,
    field: &str,
) -> Option<WorkKind> {
    let kind = value
        .get_ref()
        .as_str()
        .and_then(|s| s.parse::<WorkKind>().ok());
    if kind.is_none() {
        c.push("kind_invalid", field, Some(value.span().start));
    }
    kind
}

fn issue_kind_labels(
    c: &mut Collector<'_>,
    value: &Spanned<DeValue<'_>>,
) -> Option<BTreeMap<String, WorkKind>> {
    let field = "workflow.issue.kind_labels";
    let Some(table) = value.get_ref().as_table() else {
        c.push("field_invalid", field, Some(value.span().start));
        return None;
    };
    if table.len() > MAX_ISSUE_LABELS {
        c.push("field_invalid", field, Some(value.span().start));
        return None;
    }
    let mut out = BTreeMap::new();
    let mut valid = true;
    for (key, v) in table {
        let target = format!("{field}.{}", key.get_ref());
        if !issue_label(key.get_ref()) {
            c.push("field_invalid", &target, Some(key.span().start));
            valid = false;
            continue;
        }
        match issue_kind(c, v, &target) {
            Some(kind) => {
                out.insert(key.get_ref().to_lowercase(), kind);
            }
            None => valid = false,
        }
    }
    valid.then_some(out)
}

fn issue_bugfix(c: &mut Collector<'_>, value: &Spanned<DeValue<'_>>) -> Option<IssueBugfix> {
    let base = "workflow.issue.bugfix";
    let Some(table) = value.get_ref().as_table() else {
        c.push("field_invalid", base, Some(value.span().start));
        return None;
    };
    c.unknown(table, "workflow.issue.bugfix.", ISSUE_BUGFIX_KEYS);
    let mut found: [Vec<String>; 2] = [Vec::new(), Vec::new()];
    let mut valid = true;
    for (index, key) in ISSUE_BUGFIX_KEYS.iter().enumerate() {
        let field = format!("{base}.{key}");
        let Some(v) = get(table, key) else {
            c.push("field_missing", &field, Some(value.span().start));
            valid = false;
            continue;
        };
        let Some(items) = v.get_ref().as_array() else {
            c.push("field_invalid", &field, Some(v.span().start));
            valid = false;
            continue;
        };
        let texts: Vec<String> = items
            .iter()
            .filter_map(|i| i.get_ref().as_str().map(String::from))
            .collect();
        if texts.len() != items.len() || !(1..=MAX_ISSUE_COMMANDS).contains(&texts.len()) {
            c.push("field_invalid", &field, Some(v.span().start));
            valid = false;
            continue;
        }
        found[index] = texts;
    }
    if !valid {
        return None;
    }
    let [done_when, regression] = found;
    let probe = QuickSpec {
        kind: WorkKind::Bugfix,
        title: "issue".to_string(),
        description: String::new(),
        done_when: done_when.clone(),
        regression: regression.clone(),
        criteria: Vec::new(),
        locale: saka_spec::Locale::En,
        repo: None,
    };
    let problems: Vec<_> = check_quick(&probe)
        .into_iter()
        .filter(|i| i.field.starts_with("done_when") || i.field.starts_with("regression"))
        .collect();
    for problem in &problems {
        c.push(
            "field_invalid",
            &format!("{base}.{}", problem.field),
            Some(value.span().start),
        );
    }
    problems.is_empty().then_some(IssueBugfix {
        done_when,
        regression,
    })
}

fn issue_entry(c: &mut Collector<'_>, table: &DeTable<'_>) -> IssueEntry {
    let mut entry = IssueEntry::default();
    c.unknown(table, "workflow.issue.", ISSUE_KEYS);
    if let Some(v) = get(table, "trigger") {
        match v
            .get_ref()
            .as_str()
            .and_then(|s| s.parse::<IssueTrigger>().ok())
        {
            Some(t) => entry.trigger = t,
            None => c.push(
                "field_invalid",
                "workflow.issue.trigger",
                Some(v.span().start),
            ),
        }
    }
    if let Some(v) = get(table, "trigger_label") {
        match v.get_ref().as_str().filter(|s| issue_label(s)) {
            Some(label) => entry.trigger_label = label.to_string(),
            None => c.push(
                "field_invalid",
                "workflow.issue.trigger_label",
                Some(v.span().start),
            ),
        }
    }
    for (key, target) in [
        ("labels", &mut entry.labels),
        ("exclude_labels", &mut entry.exclude_labels),
    ] {
        if let Some(v) = get(table, key)
            && let Some(items) = issue_labels(c, v, &format!("workflow.issue.{key}"))
        {
            *target = items;
        }
    }
    if let Some(v) = get(table, "kind_labels")
        && let Some(labels) = issue_kind_labels(c, v)
    {
        entry.kind_labels = labels;
    }
    if let Some(v) = get(table, "default_kind")
        && let Some(kind) = issue_kind(c, v, "workflow.issue.default_kind")
    {
        entry.default_kind = kind;
    }
    if let Some(v) = get(table, "include_confidential")
        && let Some(b) = flag(c, v, "workflow.issue.include_confidential")
    {
        entry.include_confidential = b;
    }
    if let Some(v) = get(table, "max_open") {
        let parsed = v
            .get_ref()
            .as_integer()
            .and_then(|n| u32::try_from(n.as_str().parse::<i64>().ok()?).ok())
            .filter(|n| (1..=MAX_ISSUE_MAX_OPEN).contains(n));
        match parsed {
            Some(n) => entry.max_open = n,
            None => c.push(
                "field_invalid",
                "workflow.issue.max_open",
                Some(v.span().start),
            ),
        }
    }
    if let Some(v) = get(table, "bugfix") {
        entry.bugfix = issue_bugfix(c, v);
    }
    entry
}

fn preset_stage_allowed(preset: WorkflowPreset, stage: Stage) -> bool {
    match preset {
        WorkflowPreset::FullLoop => matches!(stage, Stage::Review | Stage::Fix | Stage::Merge),
        WorkflowPreset::BuildOnly => false,
        WorkflowPreset::ReviewOnly => matches!(stage, Stage::Review | Stage::Fix),
        WorkflowPreset::Custom => true,
    }
}

fn stages(c: &mut Collector<'_>, workflow: &mut Workflow, value: &Spanned<DeValue<'_>>) {
    let Some(table) = value.get_ref().as_table() else {
        c.push("field_invalid", "workflow.stages", Some(value.span().start));
        return;
    };
    let known: Vec<&str> = STAGE_KEYS.iter().map(|(k, _)| *k).collect();
    c.unknown(table, "workflow.stages.", &known);
    for (key, stage) in STAGE_KEYS {
        let Some(v) = get(table, key) else { continue };
        let field = format!("workflow.stages.{key}");
        let at = Some(v.span().start);
        let Some(t) = trigger(c, v, &field) else {
            continue;
        };
        let unsupported = (*stage == Stage::Dev && t == Trigger::Manual)
            || (*stage == Stage::Review && t == Trigger::Manual && workflow.spec);
        if !preset_stage_allowed(workflow.preset, *stage) {
            c.push("workflow_stage_not_allowed", &field, at);
        } else if unsupported {
            c.push("workflow_trigger_unsupported", &field, at);
        } else {
            workflow.set_stage(*stage, t);
        }
    }
}

fn fix_rounds(c: &mut Collector<'_>, workflow: &mut Workflow, value: &Spanned<DeValue<'_>>) {
    let at = Some(value.span().start);
    let rounds = value
        .get_ref()
        .as_integer()
        .and_then(|n| u32::try_from(n.as_str().parse::<i64>().ok()?).ok())
        .filter(|n| (1..=MAX_FIX_ROUNDS).contains(n));
    match rounds {
        None => c.push("field_invalid", "workflow.fix_rounds", at),
        Some(_) if !workflow.has_stage(Stage::Fix) => {
            c.push("workflow_stage_not_allowed", "workflow.fix_rounds", at);
        }
        Some(n) => workflow.fix_rounds = n,
    }
}

fn ci_reruns(c: &mut Collector<'_>, workflow: &mut Workflow, value: &Spanned<DeValue<'_>>) {
    let reruns = value
        .get_ref()
        .as_integer()
        .and_then(|n| u32::try_from(n.as_str().parse::<i64>().ok()?).ok())
        .filter(|n| *n <= MAX_CI_RERUNS);
    match reruns {
        None => c.push(
            "field_invalid",
            "workflow.ci_reruns",
            Some(value.span().start),
        ),
        Some(n) => workflow.ci_reruns = n,
    }
}

fn kind_stage(
    c: &mut Collector<'_>,
    value: &Spanned<DeValue<'_>>,
    field: &str,
) -> Option<Option<Trigger>> {
    let parsed = match value.get_ref().as_str() {
        Some("off") => Some(None),
        Some(text) => text.parse::<Trigger>().ok().map(Some),
        None => None,
    };
    if parsed.is_none() {
        c.push("field_invalid", field, Some(value.span().start));
    }
    parsed
}

fn kind_stages(
    c: &mut Collector<'_>,
    workflow: &Workflow,
    kind: WorkKind,
    over: &mut KindOverride,
    lines: &mut KindLines,
    value: &Spanned<DeValue<'_>>,
) {
    let base = format!("workflow.kinds.{kind}.stages");
    let Some(table) = value.get_ref().as_table() else {
        c.push("field_invalid", &base, Some(value.span().start));
        return;
    };
    let known: Vec<&str> = STAGE_KEYS.iter().map(|(k, _)| *k).collect();
    c.unknown(table, &format!("{base}."), &known);
    for (key, stage) in STAGE_KEYS {
        let Some(v) = get(table, key) else { continue };
        let field = format!("{base}.{key}");
        let Some(trigger) = kind_stage(c, v, &field) else {
            continue;
        };
        let unsupported = match (*stage, trigger) {
            (Stage::Dev, Some(Trigger::Manual)) => true,
            (Stage::Review, Some(Trigger::Manual)) => workflow.spec,
            _ => false,
        };
        if unsupported {
            c.push("workflow_trigger_unsupported", &field, Some(v.span().start));
            continue;
        }
        over.stages.insert(*stage, trigger);
        lines.insert((kind, *stage), line_of(c.text, v.span().start));
    }
}

fn test_paths(
    c: &mut Collector<'_>,
    kind: WorkKind,
    over: &mut KindOverride,
    value: &Spanned<DeValue<'_>>,
) {
    let field = format!("workflow.kinds.{kind}.test_paths");
    let at = Some(value.span().start);
    let Some(items) = value.get_ref().as_array() else {
        c.push("field_invalid", &field, at);
        return;
    };
    if items.len() > MAX_TEST_PATHS {
        c.push("field_invalid", &field, at);
        return;
    }
    let mut patterns = Vec::with_capacity(items.len());
    for (j, item) in items.iter().enumerate() {
        match item.get_ref().as_str() {
            Some(text) => patterns.push(text.to_string()),
            None => c.push(
                "field_invalid",
                &format!("{field}[{j}]"),
                Some(item.span().start),
            ),
        }
    }
    if patterns.len() != items.len() || patterns.is_empty() {
        over.test_paths = patterns;
        return;
    }
    match PathScope::parse(&patterns) {
        Ok(_) => over.test_paths = patterns,
        Err(issues) => {
            for issue in issues {
                let target = match issue {
                    ScopeIssue::Pattern(j) => format!("{field}[{j}]"),
                    ScopeIssue::TooMany | ScopeIssue::NoInclude => field.clone(),
                };
                c.push("kind_test_paths_invalid", &target, at);
            }
        }
    }
}

fn kinds(
    c: &mut Collector<'_>,
    workflow: &mut Workflow,
    lines: &mut KindLines,
    value: &Spanned<DeValue<'_>>,
) {
    let Some(table) = value.get_ref().as_table() else {
        c.push("field_invalid", "workflow.kinds", Some(value.span().start));
        return;
    };
    let known: Vec<&str> = WorkKind::ALL.iter().map(|k| k.as_str()).collect();
    c.unknown(table, "workflow.kinds.", &known);
    for &kind in WorkKind::ALL {
        let Some(v) = get(table, kind.as_str()) else {
            continue;
        };
        let base = format!("workflow.kinds.{kind}");
        let Some(entry) = v.get_ref().as_table() else {
            c.push("field_invalid", &base, Some(v.span().start));
            continue;
        };
        let keys = if kind == WorkKind::Bugfix {
            BUGFIX_KEYS
        } else {
            KIND_KEYS
        };
        c.unknown(entry, &format!("{base}."), keys);
        let mut over = KindOverride::default();
        if let Some(stages) = get(entry, "stages") {
            kind_stages(c, workflow, kind, &mut over, lines, stages);
        }
        over.autonomy = autonomy(c, entry, &format!("{base}.autonomy"));
        if kind == WorkKind::Bugfix
            && let Some(paths) = get(entry, "test_paths")
        {
            test_paths(c, kind, &mut over, paths);
        }
        workflow.kinds.insert(kind, over);
    }
}

pub fn kind_issue(issue: &KindIssue, lines: &KindLines) -> super::Issue {
    let stage = issue.stage.unwrap_or(Stage::Review);
    let field = match issue.refusal {
        KindRefusal::FixWithoutReview => format!("workflow.kinds.{}", issue.kind),
        _ => format!("workflow.kinds.{}.stages.{stage}", issue.kind),
    };
    super::Issue {
        code: issue.refusal.code(),
        field: Some(field),
        line: lines.get(&(issue.kind, stage)).copied(),
    }
}

pub(super) fn parse(c: &mut Collector<'_>, doc: &DeTable<'_>) -> (Workflow, KindLines) {
    let mut lines = KindLines::new();
    let Some(section) = get(doc, "workflow") else {
        return (Workflow::default(), lines);
    };
    let at = section.span().start;
    let Some(table) = section.get_ref().as_table() else {
        c.push("field_invalid", "workflow", Some(at));
        return (Workflow::default(), lines);
    };
    let before = c.issues.len();
    c.unknown(table, "workflow.", KEYS);
    let preset = match get(table, "preset") {
        None => WorkflowPreset::FullLoop,
        Some(v) => match v
            .get_ref()
            .as_str()
            .and_then(|s| s.parse::<WorkflowPreset>().ok())
        {
            Some(p) => p,
            None => {
                c.push(
                    "workflow_preset_invalid",
                    "workflow.preset",
                    Some(v.span().start),
                );
                return (Workflow::default(), lines);
            }
        },
    };
    let mut workflow = Workflow::of(preset);
    if let Some(v) = get(table, "spec") {
        match flag(c, v, "workflow.spec") {
            Some(b) if preset == WorkflowPreset::Custom => workflow.spec = b,
            Some(_) => c.push(
                "workflow_entry_not_allowed",
                "workflow.spec",
                Some(v.span().start),
            ),
            None => {}
        }
    }
    if let Some(v) = get(table, "change_request") {
        let allowed = matches!(preset, WorkflowPreset::ReviewOnly | WorkflowPreset::Custom);
        match v.get_ref().as_table() {
            None => c.push(
                "field_invalid",
                "workflow.change_request",
                Some(v.span().start),
            ),
            Some(_) if !allowed => c.push(
                "workflow_entry_not_allowed",
                "workflow.change_request",
                Some(v.span().start),
            ),
            Some(t) => workflow.change_request = Some(entry(c, t)),
        }
    }
    if let Some(v) = get(table, "issue") {
        match v.get_ref().as_table() {
            None => c.push("field_invalid", "workflow.issue", Some(v.span().start)),
            Some(_) if !workflow.spec => c.push(
                "workflow_entry_not_allowed",
                "workflow.issue",
                Some(v.span().start),
            ),
            Some(t) => workflow.issue = Some(issue_entry(c, t)),
        }
    }
    if let Some(v) = get(table, "stages") {
        stages(c, &mut workflow, v);
    }
    if let Some(v) = get(table, "fix_rounds") {
        fix_rounds(c, &mut workflow, v);
    }
    if let Some(v) = get(table, "ci_reruns") {
        ci_reruns(c, &mut workflow, v);
    }
    if let Some(v) = get(table, "kinds") {
        kinds(c, &mut workflow, &mut lines, v);
    }
    if c.issues.len() == before {
        for issue in workflow.validate() {
            c.issues.push(super::Issue {
                code: issue.code(),
                field: Some("workflow".to_string()),
                line: Some(line_of(c.text, at)),
            });
        }
        let structural = WorkflowPolicy {
            allow_unreviewed_chore: true,
        };
        for issue in workflow.kind_issues(&structural) {
            c.issues.push(kind_issue(&issue, &lines));
        }
    }
    (workflow, lines)
}

#[cfg(test)]
mod tests {
    use saka_core::vocab::{Autonomy, Stage, Trigger, WorkKind, WorkflowPreset};
    use saka_core::workflow::{DEFAULT_TRIGGER_LABEL, Workflow, WorkflowPolicy};

    use crate::project_config::parse;

    const BASE: &str =
        "[project]\nname = \"x\"\n\n[[repo]]\nname = \"a\"\nurl = \"https://github.com/acme/a\"\n";

    type Found = (&'static str, Option<String>, Option<usize>);

    fn config(workflow: &str) -> Result<Workflow, Vec<Found>> {
        parse(format!("{BASE}\n{workflow}").as_bytes())
            .map(|c| c.workflow)
            .map_err(|issues| {
                issues
                    .into_iter()
                    .map(|i| (i.code, i.field, i.line))
                    .collect()
            })
    }

    fn issues(workflow: &str) -> Vec<(&'static str, String, usize)> {
        config(workflow)
            .unwrap_err()
            .into_iter()
            .map(|(code, field, line)| (code, field.unwrap_or_default(), line.unwrap_or(0)))
            .collect()
    }

    #[test]
    fn a_config_without_a_workflow_section_is_the_full_loop() {
        assert_eq!(config("").unwrap(), Workflow::default());
        assert_eq!(config("[workflow]\n").unwrap(), Workflow::default());
    }

    #[test]
    fn review_only_takes_filters_and_triggers_from_the_file() {
        let w = config(
            r#"[workflow]
preset = "review_only"

[workflow.change_request]
trigger = "manual"
labels = ["needs-review"]
authors = ["alice", "bob"]
branches = ["feature/**", "!feature/wip/**"]
paths = ["services/api/**", "!**/*.md"]
include_forks = true
trigger_label = "run-saka"

[workflow.stages]
review = "manual"
fix = "manual"
"#,
        )
        .unwrap();
        assert_eq!(w.preset, WorkflowPreset::ReviewOnly);
        assert!(!w.spec);
        let entry = w.change_request.clone().unwrap();
        assert_eq!(entry.trigger, Trigger::Manual);
        assert_eq!(entry.labels, ["needs-review"]);
        assert_eq!(entry.authors, ["alice", "bob"]);
        assert_eq!(entry.branches, ["feature/**", "!feature/wip/**"]);
        assert_eq!(entry.paths, ["services/api/**", "!**/*.md"]);
        assert!(entry.include_forks && !entry.include_drafts);
        assert_eq!(entry.trigger_label, "run-saka");
        assert_eq!(w.stage_trigger(Stage::Review), Some(Trigger::Manual));
        assert_eq!(w.stage_trigger(Stage::Fix), Some(Trigger::Manual));
    }

    #[test]
    fn the_fix_rounds_default_to_two_and_take_one_to_ten() {
        assert_eq!(config("").unwrap().fix_rounds, 2);
        assert_eq!(
            config("[workflow]\nfix_rounds = 4\n").unwrap().fix_rounds,
            4
        );
        for bad in ["0", "11", "-1", "\"two\"", "2.5"] {
            assert_eq!(
                issues(&format!("[workflow]\nfix_rounds = {bad}\n")),
                [("field_invalid", "workflow.fix_rounds".to_string(), 9)],
                "{bad}"
            );
        }
    }

    #[test]
    fn the_ci_reruns_default_to_one_and_take_zero_to_three() {
        assert_eq!(config("").unwrap().ci_reruns, 1);
        for good in 0..=3 {
            assert_eq!(
                config(&format!("[workflow]\nci_reruns = {good}\n"))
                    .unwrap()
                    .ci_reruns,
                good
            );
        }
        for bad in ["4", "-1", "\"one\"", "1.5"] {
            assert_eq!(
                issues(&format!("[workflow]\nci_reruns = {bad}\n")),
                [("field_invalid", "workflow.ci_reruns".to_string(), 9)],
                "{bad}"
            );
        }
    }

    #[test]
    fn ci_reruns_are_allowed_in_every_preset() {
        let w = config("[workflow]\npreset = \"build_only\"\nci_reruns = 0\n").unwrap();
        assert_eq!(w.ci_reruns, 0);
    }

    #[test]
    fn fix_rounds_without_a_fix_stage_are_refused() {
        assert_eq!(
            issues("[workflow]\npreset = \"build_only\"\nfix_rounds = 3\n"),
            [(
                "workflow_stage_not_allowed",
                "workflow.fix_rounds".to_string(),
                10
            )]
        );
    }

    #[test]
    fn review_only_without_a_change_request_table_uses_the_defaults() {
        let w = config("[workflow]\npreset = \"review_only\"\n").unwrap();
        assert_eq!(w, Workflow::of(WorkflowPreset::ReviewOnly));
        assert_eq!(
            w.change_request.unwrap().trigger_label,
            DEFAULT_TRIGGER_LABEL
        );
    }

    #[test]
    fn custom_chooses_its_entries_and_stages() {
        let w = config(
            "[workflow]\npreset = \"custom\"\nspec = true\n\n[workflow.change_request]\n\n[workflow.stages]\ndev = \"auto\"\nreview = \"auto\"\nmerge = \"manual\"\n",
        )
        .unwrap();
        assert!(w.spec && w.change_request.is_some());
        assert_eq!(
            w.stages.iter().map(|s| s.stage).collect::<Vec<_>>(),
            [Stage::Dev, Stage::Review, Stage::Merge]
        );
    }

    #[test]
    fn full_loop_adjusts_triggers_and_build_only_takes_none() {
        let w = config("[workflow]\n[workflow.stages]\nfix = \"manual\"\n").unwrap();
        assert_eq!(w.stage_trigger(Stage::Fix), Some(Trigger::Manual));
        assert_eq!(w.stage_trigger(Stage::Merge), Some(Trigger::Auto));
        let narrowed = config("[workflow]\n[workflow.stages]\nmerge = \"manual\"\n").unwrap();
        assert_eq!(narrowed.stage_trigger(Stage::Merge), Some(Trigger::Manual));
        let custom = config(
            "[workflow]\npreset = \"custom\"\nspec = true\n[workflow.stages]\ndev = \"auto\"\nmerge = \"auto\"\n",
        )
        .unwrap();
        assert_eq!(custom.stage_trigger(Stage::Merge), Some(Trigger::Auto));
        assert_eq!(
            issues("[workflow]\npreset = \"build_only\"\n[workflow.stages]\nreview = \"auto\"\n"),
            [(
                "workflow_stage_not_allowed",
                "workflow.stages.review".into(),
                11
            )]
        );
    }

    #[test]
    fn entries_and_stages_outside_the_preset_are_refused_at_their_line() {
        assert_eq!(
            issues("[workflow]\n[workflow.change_request]\n"),
            [(
                "workflow_entry_not_allowed",
                "workflow.change_request".into(),
                9
            )]
        );
        assert_eq!(
            issues("[workflow]\npreset = \"review_only\"\nspec = true\n"),
            [("workflow_entry_not_allowed", "workflow.spec".into(), 10)]
        );
        assert_eq!(
            issues("[workflow]\npreset = \"review_only\"\n[workflow.stages]\nmerge = \"manual\"\n"),
            [(
                "workflow_stage_not_allowed",
                "workflow.stages.merge".into(),
                11
            )]
        );
    }

    #[test]
    fn triggers_with_no_consumer_are_refused_instead_of_ignored() {
        assert_eq!(
            issues(
                "[workflow]\npreset = \"custom\"\nspec = true\n[workflow.stages]\ndev = \"manual\"\n"
            ),
            [(
                "workflow_trigger_unsupported",
                "workflow.stages.dev".into(),
                12
            )]
        );
    }

    #[test]
    fn a_manual_review_is_refused_while_specs_are_an_entry_but_fine_for_outside_changes() {
        assert_eq!(
            issues(
                "[workflow]\npreset = \"custom\"\nspec = true\n[workflow.stages]\ndev = \"auto\"\nreview = \"manual\"\n"
            ),
            [(
                "workflow_trigger_unsupported",
                "workflow.stages.review".into(),
                13
            )]
        );
        let outside = config(
            "[workflow]\npreset = \"review_only\"\n[workflow.stages]\nreview = \"manual\"\n",
        )
        .unwrap();
        assert_eq!(outside.stage_trigger(Stage::Review), Some(Trigger::Manual));
    }

    #[test]
    fn meaningless_combinations_name_the_core_rule_they_break() {
        let code = |text: &str| issues(text).into_iter().map(|i| i.0).collect::<Vec<_>>();
        assert_eq!(
            code("[workflow]\npreset = \"custom\"\n"),
            ["workflow_no_entry"]
        );
        assert_eq!(
            code(
                "[workflow]\npreset = \"custom\"\nspec = true\n[workflow.stages]\nreview = \"auto\"\n"
            ),
            ["workflow_spec_without_dev"]
        );
        assert_eq!(
            code(
                "[workflow]\npreset = \"custom\"\n[workflow.change_request]\n[workflow.stages]\nfix = \"manual\"\n"
            ),
            [
                "workflow_fix_without_review",
                "workflow_change_request_needs_review"
            ]
        );
        assert_eq!(
            code(
                "[workflow]\npreset = \"custom\"\n[workflow.change_request]\n[workflow.stages]\ndev = \"auto\"\nreview = \"auto\"\n"
            ),
            ["workflow_dev_without_spec"]
        );
    }

    #[test]
    fn bad_values_are_reported_together_with_their_fields() {
        let found = issues(
            r#"[workflow]
preset = "review_only"
color = "red"

[workflow.change_request]
trigger = "sometimes"
labels = ["", 3]
branches = ["/abs"]
paths = ["!only/excluded/**"]
include_forks = "yes"
trigger_label = ""
mystery = 1

[workflow.stages]
review = "later"
sync = "auto"
"#,
        );
        assert_eq!(
            found,
            [
                ("field_unknown", "workflow.color".into(), 10),
                (
                    "field_invalid",
                    "workflow.change_request.trigger".into(),
                    13
                ),
                (
                    "field_invalid",
                    "workflow.change_request.labels[0]".into(),
                    14
                ),
                (
                    "field_invalid",
                    "workflow.change_request.labels[1]".into(),
                    14
                ),
                (
                    "field_invalid",
                    "workflow.change_request.branches".into(),
                    15
                ),
                (
                    "path_scope_empty",
                    "workflow.change_request.paths".into(),
                    16
                ),
                (
                    "field_invalid",
                    "workflow.change_request.include_forks".into(),
                    17
                ),
                (
                    "field_invalid",
                    "workflow.change_request.trigger_label".into(),
                    18
                ),
                (
                    "field_unknown",
                    "workflow.change_request.mystery".into(),
                    19
                ),
                ("field_invalid", "workflow.stages.review".into(), 22),
                ("field_unknown", "workflow.stages.sync".into(), 23),
            ]
        );
        assert_eq!(
            issues("[workflow]\npreset = \"nope\"\n"),
            [("workflow_preset_invalid", "workflow.preset".into(), 9)]
        );
    }

    #[test]
    fn filter_lists_are_bounded() {
        let many = (0..17)
            .map(|n| format!("\"l{n}\""))
            .collect::<Vec<_>>()
            .join(", ");
        assert_eq!(
            issues(&format!(
                "[workflow]\npreset = \"review_only\"\n[workflow.change_request]\nlabels = [{many}]\n"
            )),
            [("field_invalid", "workflow.change_request.labels".into(), 11)]
        );
        let long = "x".repeat(101);
        assert_eq!(
            issues(&format!(
                "[workflow]\npreset = \"review_only\"\n[workflow.change_request]\nauthors = [\"{long}\"]\n"
            )),
            [(
                "field_invalid",
                "workflow.change_request.authors[0]".into(),
                11
            )]
        );
    }

    #[test]
    fn kind_overrides_are_read_into_the_workflow() {
        let w = config(
            r#"[workflow.kinds.bugfix]
stages = { review = "auto", merge = "manual" }
autonomy = "standard"
test_paths = ["src/**/fixtures/**", "!src/skip/**"]

[workflow.kinds.chore]
stages = { review = "off" }
"#,
        )
        .unwrap();
        let bugfix = &w.kinds[&WorkKind::Bugfix];
        assert_eq!(
            bugfix
                .stages
                .iter()
                .map(|(s, t)| (*s, *t))
                .collect::<Vec<_>>(),
            [
                (Stage::Review, Some(Trigger::Auto)),
                (Stage::Merge, Some(Trigger::Manual))
            ]
        );
        assert_eq!(bugfix.autonomy, Some(Autonomy::Standard));
        assert_eq!(bugfix.test_paths, ["src/**/fixtures/**", "!src/skip/**"]);
        assert_eq!(w.kinds[&WorkKind::Chore].stages[&Stage::Review], None);
        assert_eq!(w.kinds.len(), 2);
        assert_eq!(w.stages, Workflow::default().stages);
    }

    #[test]
    fn unknown_kinds_and_keys_are_named() {
        assert_eq!(
            issues("[workflow.kinds.hotfix]\nstages = {}\n"),
            [("field_unknown", "workflow.kinds.hotfix".into(), 8)]
        );
        assert_eq!(
            issues("[workflow.kinds.bugfix]\nretries = 2\n"),
            [("field_unknown", "workflow.kinds.bugfix.retries".into(), 9)]
        );
        assert_eq!(
            issues("[workflow.kinds.chore]\ntest_paths = [\"a/**\"]\n"),
            [("field_unknown", "workflow.kinds.chore.test_paths".into(), 9)]
        );
        assert_eq!(
            issues("[workflow.kinds.bugfix]\nstages = { sync = \"auto\" }\n"),
            [(
                "field_unknown",
                "workflow.kinds.bugfix.stages.sync".into(),
                9
            )]
        );
    }

    #[test]
    fn kind_values_must_have_the_documented_shape() {
        for (text, field, line) in [
            (
                "[workflow.kinds.bugfix]\nstages = { review = \"sometimes\" }\n",
                "workflow.kinds.bugfix.stages.review",
                9,
            ),
            (
                "[workflow.kinds.bugfix]\nstages = \"auto\"\n",
                "workflow.kinds.bugfix.stages",
                9,
            ),
            (
                "[workflow.kinds.bugfix]\nautonomy = \"free\"\n",
                "workflow.kinds.bugfix.autonomy",
                9,
            ),
            (
                "[workflow.kinds.bugfix]\ntest_paths = \"tests/**\"\n",
                "workflow.kinds.bugfix.test_paths",
                9,
            ),
            ("[workflow.kinds]\nbugfix = 3\n", "workflow.kinds.bugfix", 9),
            ("[workflow]\nkinds = 3\n", "workflow.kinds", 9),
        ] {
            assert_eq!(
                issues(text),
                [("field_invalid", field.to_string(), line)],
                "{text}"
            );
        }
    }

    #[test]
    fn test_paths_are_valid_patterns_and_at_most_thirty_two() {
        assert_eq!(
            issues("[workflow.kinds.bugfix]\ntest_paths = [\"ok/**\", \"[bad\"]\n"),
            [(
                "kind_test_paths_invalid",
                "workflow.kinds.bugfix.test_paths[1]".into(),
                9
            )]
        );
        assert_eq!(
            issues("[workflow.kinds.bugfix]\ntest_paths = [\"!only/excludes/**\"]\n"),
            [(
                "kind_test_paths_invalid",
                "workflow.kinds.bugfix.test_paths".into(),
                9
            )]
        );
        let many = vec!["\"a/**\""; 33].join(", ");
        assert_eq!(
            issues(&format!("[workflow.kinds.bugfix]\ntest_paths = [{many}]\n")),
            [(
                "field_invalid",
                "workflow.kinds.bugfix.test_paths".into(),
                9
            )]
        );
        let fits = vec!["\"a/**\""; 32].join(", ");
        assert!(config(&format!("[workflow.kinds.bugfix]\ntest_paths = [{fits}]\n")).is_ok());
        assert!(config("[workflow.kinds.bugfix]\ntest_paths = []\n").is_ok());
    }

    #[test]
    fn kind_overrides_that_break_a_rule_name_it_at_their_line() {
        assert_eq!(
            issues("[workflow.kinds.bugfix]\nstages = { review = \"off\" }\n"),
            [(
                "kind_review_required",
                "workflow.kinds.bugfix.stages.review".into(),
                9
            )]
        );
        assert_eq!(
            issues("[workflow.kinds.security_patch]\nstages = { review = \"off\" }\n"),
            [(
                "kind_review_required",
                "workflow.kinds.security_patch.stages.review".into(),
                9
            )]
        );
        assert_eq!(
            issues("[workflow.kinds.refactor]\nstages = { dev = \"off\" }\n"),
            [(
                "kind_stage_required",
                "workflow.kinds.refactor.stages.dev".into(),
                9
            )]
        );
        assert_eq!(
            issues(
                "[workflow]\npreset = \"build_only\"\n[workflow.kinds.refactor]\nstages = { merge = \"auto\" }\n"
            ),
            [(
                "workflow_stage_not_allowed",
                "workflow.kinds.refactor.stages.merge".into(),
                11
            )]
        );
        assert_eq!(
            issues(
                "[workflow]\npreset = \"build_only\"\n[workflow.kinds.refactor]\nstages = { fix = \"auto\" }\n"
            ),
            [(
                "workflow_fix_without_review",
                "workflow.kinds.refactor".into(),
                11
            )]
        );
        assert_eq!(
            issues("[workflow.kinds.refactor]\nstages = { dev = \"manual\" }\n"),
            [(
                "workflow_trigger_unsupported",
                "workflow.kinds.refactor.stages.dev".into(),
                9
            )]
        );
        assert_eq!(
            issues("[workflow.kinds.refactor]\nstages = { review = \"manual\" }\n"),
            [(
                "workflow_trigger_unsupported",
                "workflow.kinds.refactor.stages.review".into(),
                9
            )]
        );
    }

    #[test]
    fn a_chore_without_review_passes_the_file_check_and_waits_for_the_operator_check() {
        let w = config("[workflow.kinds.chore]\nstages = { review = \"off\" }\n").unwrap();
        assert_eq!(
            w.kind_issues(&WorkflowPolicy::default())
                .iter()
                .map(|i| i.refusal.code())
                .collect::<Vec<_>>(),
            ["kind_review_not_allowed"]
        );
    }

    const ISSUE_FULL: &str = r#"[workflow.issue]
trigger = "auto"
trigger_label = "run-saka"
labels = ["triage"]
exclude_labels = ["wontfix"]
kind_labels = { Bug = "bugfix", docs = "chore" }
default_kind = "refactor"
include_confidential = true
max_open = 5

[workflow.issue.bugfix]
done_when = ["cargo test --locked"]
regression = ["cargo test --locked bug"]
"#;

    #[test]
    fn an_issue_entry_takes_every_field_from_the_file() {
        use saka_core::vocab::IssueTrigger;
        let w = config(ISSUE_FULL).unwrap();
        let entry = w.issue.unwrap();
        assert_eq!(entry.trigger, IssueTrigger::Auto);
        assert_eq!(entry.trigger_label, "run-saka");
        assert_eq!(entry.labels, ["triage"]);
        assert_eq!(entry.exclude_labels, ["wontfix"]);
        assert_eq!(
            entry.kind_labels.into_iter().collect::<Vec<_>>(),
            [
                ("bug".to_string(), WorkKind::Bugfix),
                ("docs".to_string(), WorkKind::Chore)
            ]
        );
        assert_eq!(entry.default_kind, WorkKind::Refactor);
        assert!(entry.include_confidential);
        assert_eq!(entry.max_open, 5);
        let bugfix = entry.bugfix.unwrap();
        assert_eq!(bugfix.done_when, ["cargo test --locked"]);
        assert_eq!(bugfix.regression, ["cargo test --locked bug"]);
    }

    #[test]
    fn an_empty_issue_entry_has_the_documented_defaults() {
        use saka_core::vocab::IssueTrigger;
        let entry = config("[workflow.issue]\n").unwrap().issue.unwrap();
        assert_eq!(entry.trigger, IssueTrigger::Label);
        assert_eq!(entry.trigger_label, "saka:intake");
        assert_eq!(entry.exclude_labels, ["wontfix", "duplicate", "question"]);
        assert_eq!(entry.kind_labels["bug"], WorkKind::Bugfix);
        assert_eq!(entry.kind_labels["security"], WorkKind::SecurityPatch);
        assert_eq!(entry.default_kind, WorkKind::Feature);
        assert_eq!((entry.max_open, entry.include_confidential), (20, false));
        assert!(entry.bugfix.is_none());
        assert!(config("").unwrap().issue.is_none());
    }

    #[test]
    fn an_issue_entry_needs_a_workflow_that_takes_specs() {
        let review_only = "[workflow]\npreset = \"review_only\"\n\n[workflow.issue]\n";
        assert!(
            issues(review_only)
                .iter()
                .any(|(code, field, _)| *code == "workflow_entry_not_allowed"
                    && field == "workflow.issue")
        );
        let build_only = "[workflow]\npreset = \"build_only\"\n\n[workflow.issue]\n";
        assert!(config(build_only).unwrap().issue.is_some());
        let custom = "[workflow]\npreset = \"custom\"\nspec = false\n\n[workflow.issue]\n";
        assert!(
            issues(custom)
                .iter()
                .any(|(code, field, _)| *code == "workflow_entry_not_allowed"
                    && field == "workflow.issue")
        );
    }

    #[test]
    fn an_unknown_kind_is_kind_invalid_and_other_keys_are_unknown() {
        assert_eq!(
            issues("[workflow.issue]\nkind_labels = { bug = \"hotfix\" }\n"),
            [("kind_invalid", "workflow.issue.kind_labels.bug".into(), 9)]
        );
        assert_eq!(
            issues("[workflow.issue]\ndefault_kind = \"epic\"\n"),
            [("kind_invalid", "workflow.issue.default_kind".into(), 9)]
        );
        assert_eq!(
            issues("[workflow.issue]\nreviewers = 2\n"),
            [("field_unknown", "workflow.issue.reviewers".into(), 9)]
        );
        assert_eq!(
            issues(
                "[workflow.issue.bugfix]\ndone_when = [\"a\"]\nregression = [\"b\"]\nretries = 1\n"
            )[0]
            .0,
            "field_unknown"
        );
    }

    #[test]
    fn issue_values_are_range_checked() {
        for (line, field) in [
            ("trigger = \"manual\"", "workflow.issue.trigger"),
            ("trigger_label = \"\"", "workflow.issue.trigger_label"),
            ("trigger_label = \"a,b\"", "workflow.issue.trigger_label"),
            ("max_open = 0", "workflow.issue.max_open"),
            ("max_open = 101", "workflow.issue.max_open"),
            ("labels = [\"\"]", "workflow.issue.labels[0]"),
            (
                "include_confidential = \"yes\"",
                "workflow.issue.include_confidential",
            ),
        ] {
            let found = issues(&format!("[workflow.issue]\n{line}\n"));
            assert_eq!(found[0].0, "field_invalid", "{line}");
            assert_eq!(found[0].1, field, "{line}");
        }
        let many = (0..21)
            .map(|n| format!("\"l{n}\""))
            .collect::<Vec<_>>()
            .join(", ");
        assert_eq!(
            issues(&format!("[workflow.issue]\nlabels = [{many}]\n"))[0].1,
            "workflow.issue.labels"
        );
    }

    #[test]
    fn the_bugfix_lane_needs_both_command_lists_within_bounds() {
        let missing = issues("[workflow.issue.bugfix]\ndone_when = [\"a\"]\n");
        assert_eq!(missing[0].0, "field_missing");
        assert_eq!(missing[0].1, "workflow.issue.bugfix.regression");
        let empty = issues("[workflow.issue.bugfix]\ndone_when = []\nregression = [\"b\"]\n");
        assert_eq!(empty[0].1, "workflow.issue.bugfix.done_when");
        let many = (0..11)
            .map(|n| format!("\"c{n}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let over = issues(&format!(
            "[workflow.issue.bugfix]\ndone_when = [{many}]\nregression = [\"b\"]\n"
        ));
        assert_eq!(over[0].1, "workflow.issue.bugfix.done_when");
        let multiline =
            issues("[workflow.issue.bugfix]\ndone_when = [\"a\\nb\"]\nregression = [\"b\"]\n");
        assert_eq!(multiline[0].1, "workflow.issue.bugfix.done_when[0]");
    }
}
