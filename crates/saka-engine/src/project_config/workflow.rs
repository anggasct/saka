use saka_core::scope::{PathScope, ScopeIssue};
use saka_core::vocab::{Stage, Trigger, WorkflowPreset};
use saka_core::workflow::{
    ChangeEntry, MAX_FILTER_ENTRIES, MAX_FIX_ROUNDS, Workflow, filter_text_ok,
};
use toml::Spanned;
use toml::de::{DeTable, DeValue};

use super::{Collector, get, line_of};

const KEYS: &[&str] = &["preset", "spec", "change_request", "stages", "fix_rounds"];
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
        let unsupported = (*stage == Stage::Merge && t == Trigger::Auto)
            || (*stage == Stage::Dev && t == Trigger::Manual)
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

pub(super) fn parse(c: &mut Collector<'_>, doc: &DeTable<'_>) -> Workflow {
    let Some(section) = get(doc, "workflow") else {
        return Workflow::default();
    };
    let at = section.span().start;
    let Some(table) = section.get_ref().as_table() else {
        c.push("field_invalid", "workflow", Some(at));
        return Workflow::default();
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
                return Workflow::default();
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
    if let Some(v) = get(table, "stages") {
        stages(c, &mut workflow, v);
    }
    if let Some(v) = get(table, "fix_rounds") {
        fix_rounds(c, &mut workflow, v);
    }
    if c.issues.len() == before {
        for issue in workflow.validate() {
            c.issues.push(super::Issue {
                code: issue.code(),
                field: Some("workflow".to_string()),
                line: Some(line_of(c.text, at)),
            });
        }
    }
    workflow
}

#[cfg(test)]
mod tests {
    use saka_core::vocab::{Stage, Trigger, WorkflowPreset};
    use saka_core::workflow::{DEFAULT_TRIGGER_LABEL, Workflow};

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
        assert_eq!(w.stage_trigger(Stage::Merge), Some(Trigger::Manual));
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
            issues("[workflow]\n[workflow.stages]\nmerge = \"auto\"\n"),
            [(
                "workflow_trigger_unsupported",
                "workflow.stages.merge".into(),
                10
            )]
        );
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
}
