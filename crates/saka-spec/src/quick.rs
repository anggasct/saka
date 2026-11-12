use std::fmt::Write;

use saka_core::vocab::WorkKind;

use crate::model::SpecModel;
use crate::parse::MAX_REGRESSION;
use crate::template::Locale;

pub const MAX_QUICK_TITLE: usize = 120;
pub const MAX_QUICK_DESCRIPTION: usize = 4000;
pub const MAX_QUICK_COMMANDS: usize = 10;
pub const MAX_QUICK_COMMAND: usize = 500;
pub const MAX_QUICK_CRITERIA: usize = 10;
pub const MAX_QUICK_CRITERION: usize = 500;
const MAX_SLUG: usize = 63;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuickSpec {
    pub kind: WorkKind,
    pub title: String,
    pub description: String,
    pub done_when: Vec<String>,
    pub regression: Vec<String>,
    pub criteria: Vec<String>,
    pub locale: Locale,
    pub repo: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuickIssue {
    pub field: String,
    pub code: &'static str,
}

fn issue(field: impl Into<String>, code: &'static str) -> QuickIssue {
    QuickIssue {
        field: field.into(),
        code,
    }
}

fn single_line(text: &str) -> bool {
    !text.chars().any(char::is_control)
}

fn line_ok(field: String, text: &str, max: usize, issues: &mut Vec<QuickIssue>) {
    if text.trim().is_empty() {
        issues.push(issue(field, "required"));
    } else if text.chars().count() > max {
        issues.push(issue(field, "too_long"));
    } else if !single_line(text) {
        issues.push(issue(field, "invalid"));
    }
}

fn commands_ok(
    field: &str,
    items: &[String],
    max: usize,
    min: usize,
    issues: &mut Vec<QuickIssue>,
) {
    if items.len() < min {
        issues.push(issue(field, "required"));
    }
    if items.len() > max {
        issues.push(issue(field, "too_many"));
        return;
    }
    for (i, item) in items.iter().enumerate() {
        line_ok(format!("{field}[{i}]"), item, MAX_QUICK_COMMAND, issues);
    }
}

pub fn check(spec: &QuickSpec) -> Vec<QuickIssue> {
    let mut issues = Vec::new();
    line_ok("title".into(), &spec.title, MAX_QUICK_TITLE, &mut issues);
    if spec.description.chars().count() > MAX_QUICK_DESCRIPTION {
        issues.push(issue("description", "too_long"));
    }
    commands_ok(
        "done_when",
        &spec.done_when,
        MAX_QUICK_COMMANDS,
        1,
        &mut issues,
    );
    if spec.kind == WorkKind::Bugfix {
        commands_ok(
            "regression",
            &spec.regression,
            MAX_REGRESSION,
            0,
            &mut issues,
        );
    } else if !spec.regression.is_empty() {
        issues.push(issue("regression", "not_allowed"));
    }
    if spec.criteria.len() > MAX_QUICK_CRITERIA {
        issues.push(issue("ac", "too_many"));
    } else {
        for (i, text) in spec.criteria.iter().enumerate() {
            line_ok(format!("ac[{i}]"), text, MAX_QUICK_CRITERION, &mut issues);
        }
    }
    issues
}

pub fn slug(kind: WorkKind, title: &str) -> Option<String> {
    let mut body = String::new();
    let mut pending = false;
    for c in title.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            if pending && !body.is_empty() {
                body.push('-');
            }
            pending = false;
            body.push(c);
        } else {
            pending = true;
        }
    }
    if body.is_empty() {
        return None;
    }
    let prefix = match kind {
        WorkKind::Feature => String::new(),
        other => format!("{}-", other.as_str().replace('_', "-")),
    };
    let room = MAX_SLUG.saturating_sub(prefix.len());
    let body = body.chars().take(room).collect::<String>();
    Some(format!("{prefix}{}", body.trim_end_matches('-')))
}

fn default_criterion(spec: &QuickSpec) -> String {
    let title = spec.title.trim();
    match (spec.locale, spec.kind) {
        (Locale::En, WorkKind::Bugfix) => format!(
            "{title}: the regression commands fail before the fix, and every command in Done when passes after it."
        ),
        (Locale::En, _) => format!("{title}: every command in Done when passes."),
        (Locale::Id, WorkKind::Bugfix) => format!(
            "{title}: perintah regresi gagal sebelum perbaikan, dan semua perintah di Done when lulus sesudahnya."
        ),
        (Locale::Id, _) => format!("{title}: semua perintah di Done when lulus."),
    }
}

const INLINE_SPECIALS: &[char] = &['\\', '`', '*', '_', '[', ']', '<', '>', '&', '|', '~', '#'];
const BLOCK_STARTS: &[char] = &[
    '#', '>', '|', '~', '`', '+', '*', '-', '<', '=', ':', '.', '[', '_',
];

fn escape_inline(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.trim().chars() {
        if INLINE_SPECIALS.contains(&c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn escape_block_line(line: &str) -> String {
    let line = line.trim();
    let Some(first) = line.chars().next() else {
        return String::new();
    };
    if BLOCK_STARTS.contains(&first) {
        return format!("\\{line}");
    }
    let digits = line.chars().take_while(char::is_ascii_digit).count();
    let marked = line[digits..].starts_with(['.', ')']);
    if digits > 0 && marked {
        return format!("{}\\{}", &line[..digits], &line[digits..]);
    }
    line.to_string()
}

fn plain_text(text: &str) -> String {
    let mut out = String::new();
    let mut blank = false;
    for line in text
        .trim()
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .lines()
    {
        let escaped = escape_block_line(line);
        if escaped.is_empty() {
            blank = true;
            continue;
        }
        if !out.is_empty() {
            out.push_str(if blank { "\n\n" } else { "\n" });
        }
        blank = false;
        out.push_str(&escaped);
    }
    out
}

fn code_span(command: &str, in_table: bool) -> String {
    let command = command.trim();
    let longest = command
        .split(|c| c != '`')
        .map(str::len)
        .max()
        .unwrap_or_default();
    let fence = "`".repeat(longest + 1);
    let body = if in_table {
        command.replace('|', "\\|")
    } else {
        command.to_string()
    };
    let pad = if body.starts_with('`') || body.ends_with('`') {
        " "
    } else {
        ""
    };
    format!("{fence}{pad}{body}{pad}{fence}")
}

fn code_spans(commands: &[String], in_table: bool) -> String {
    commands
        .iter()
        .map(|c| code_span(c, in_table))
        .collect::<Vec<_>>()
        .join(" ")
}

fn criteria_of(spec: &QuickSpec) -> Vec<String> {
    if spec.criteria.is_empty() {
        vec![default_criterion(spec)]
    } else {
        spec.criteria.iter().map(|c| c.trim().to_string()).collect()
    }
}

pub fn render(spec: &QuickSpec) -> String {
    let title = escape_inline(&spec.title);
    let criteria = criteria_of(spec);
    let mut out = format!("---\nsaka: spec\nkind: {}\n---\n# {title}\n", spec.kind);
    let description = plain_text(&spec.description);
    if !description.is_empty() {
        let _ = write!(out, "\n{description}\n");
    }
    out.push_str("\n## Acceptance criteria\n\n");
    for (i, text) in criteria.iter().enumerate() {
        let _ = writeln!(out, "- **AC-{:02}**: {}", i + 1, escape_inline(text));
    }
    if spec.kind == WorkKind::Bugfix {
        out.push_str("\n## Regression\n\n");
        for command in &spec.regression {
            let _ = writeln!(out, "- {}", code_span(command, false));
        }
    }
    match spec.repo.as_deref() {
        None => {
            out.push_str("\n## Done when\n\n");
            for command in &spec.done_when {
                let _ = writeln!(out, "- {}", code_span(command, false));
            }
        }
        Some(repo) => {
            let ids = (1..=criteria.len())
                .map(|n| format!("AC-{n:02}"))
                .collect::<Vec<_>>()
                .join(", ");
            let _ = write!(
                out,
                "\n## Steps\n\n| Step | Work | AC | Done when | Repo |\n|------|------|----|-----------|------|\n| S1 | {} | {ids} | {} | {repo} |\n",
                escape_inline(&spec.title),
                code_spans(&spec.done_when, true)
            );
        }
    }
    out
}

pub fn keeps_structure(spec: &QuickSpec, model: &SpecModel) -> bool {
    let commands = |items: &[String]| {
        items
            .iter()
            .map(|c| c.trim().to_string())
            .collect::<Vec<_>>()
    };
    let criteria = criteria_of(spec);
    let [step] = model.steps.as_slice() else {
        return false;
    };
    model.kind == spec.kind
        && model.title == spec.title.trim()
        && step.commands == commands(&spec.done_when)
        && model.regression == commands(&spec.regression)
        && model
            .acceptance_criteria
            .iter()
            .map(|a| a.text.as_str())
            .eq(criteria.iter().map(String::as_str))
        && step.ac.len() == criteria.len()
        && step.repo.as_deref() == spec.repo.as_deref()
        && model.depends_on.is_empty()
        && model.decisions.is_empty()
}
