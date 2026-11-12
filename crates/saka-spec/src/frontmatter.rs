use std::collections::HashSet;

use saka_core::vocab::WorkKind;

use crate::lint::{LintCode, LintIssue};
use crate::model::{MAX_PRIORITY, MIN_PRIORITY};
use crate::parse::valid_slug;

const KNOWN_KEYS: [&str; 4] = ["saka", "depends_on", "priority", "kind"];

pub(crate) struct Frontmatter {
    pub is_spec: bool,
    pub end_line: u32,
    pub body_offset: usize,
    pub priority: i32,
    pub kind: WorkKind,
    pub depends_on: Vec<String>,
    pub depends_on_line: Option<u32>,
    pub issues: Vec<LintIssue>,
}

struct Entry<'t> {
    key: &'t str,
    line: u32,
    value: &'t str,
    nested: Vec<(u32, &'t str)>,
}

pub(crate) fn read(text: &str, slug: &str) -> Option<Frontmatter> {
    let mut offset = 0;
    let mut lines = text.split_inclusive('\n').map(|raw| {
        let start = offset;
        offset += raw.len();
        (start + raw.len(), raw.trim_end_matches(['\n', '\r']))
    });
    if lines.next()?.1.trim_end() != "---" {
        return None;
    }

    let mut issues = Vec::new();
    let mut entries: Vec<Entry<'_>> = Vec::new();
    let mut close = None;
    for (n, (end, raw)) in lines.enumerate() {
        let line = u32::try_from(n + 2).unwrap_or(u32::MAX);
        if matches!(raw.trim_end(), "---" | "...") {
            close = Some((line, end));
            break;
        }
        let content = strip_comment(raw);
        if content.trim().is_empty() {
            continue;
        }
        if raw.starts_with([' ', '\t']) {
            match entries.last_mut() {
                Some(entry) => entry.nested.push((line, content.trim())),
                None => issues.push(not_a_key(line, content.trim())),
            }
            continue;
        }
        match split_key(content) {
            Some((key, value)) => {
                entries.push(Entry {
                    key,
                    line,
                    value,
                    nested: Vec::new(),
                });
            }
            None => {
                issues.push(not_a_key(line, content.trim()));
                entries.push(Entry {
                    key: "",
                    line,
                    value: "",
                    nested: Vec::new(),
                });
            }
        }
    }
    let (end_line, body_offset) = close?;

    let mut front = Frontmatter {
        is_spec: false,
        end_line,
        body_offset,
        priority: 0,
        kind: WorkKind::default(),
        depends_on: Vec::new(),
        depends_on_line: None,
        issues,
    };
    let mut seen = HashSet::new();
    for entry in entries.iter().filter(|e| KNOWN_KEYS.contains(&e.key)) {
        if !seen.insert(entry.key) {
            front.issues.push(
                LintIssue::new(LintCode::FrontmatterInvalid, entry.line)
                    .with("reason", "duplicate_key")
                    .with("key", entry.key),
            );
            continue;
        }
        match entry.key {
            "saka" => front.is_spec = unquote(entry.value) == "spec",
            "priority" => read_priority(entry, &mut front),
            "kind" => read_kind(entry, &mut front),
            _ => read_depends_on(entry, slug, &mut front),
        }
    }
    Some(front)
}

pub(crate) fn top_level(text: &str) -> Option<Vec<(&str, String)>> {
    let mut lines = text.split('\n').map(|l| l.trim_end_matches('\r'));
    if lines.next()?.trim_end() != "---" {
        return None;
    }
    let mut pairs = Vec::new();
    for raw in lines {
        if matches!(raw.trim_end(), "---" | "...") {
            return Some(pairs);
        }
        if raw.starts_with([' ', '\t']) {
            continue;
        }
        if let Some((key, value)) = split_key(strip_comment(raw)) {
            pairs.push((key, unquote(value).to_string()));
        }
    }
    None
}

fn not_a_key(line: u32, content: &str) -> LintIssue {
    let key = content.split_once(':').map_or(content, |(key, _)| key);
    LintIssue::new(LintCode::FrontmatterInvalid, line)
        .with("reason", "not_a_key")
        .with("key", key.trim())
}

fn split_key(line: &str) -> Option<(&str, &str)> {
    let (key, value) = line.split_once(':')?;
    let mut chars = key.chars();
    let first = chars.next()?;
    let valid = (first.is_ascii_lowercase() || first == '_')
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-');
    let separated = value.is_empty() || value.starts_with([' ', '\t']);
    (valid && separated).then_some((key, value.trim()))
}

fn strip_comment(line: &str) -> &str {
    let mut quote = None;
    let mut previous = ' ';
    for (i, c) in line.char_indices() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if c == '\'' || c == '"' => quote = Some(c),
            None if c == '#' && previous.is_whitespace() => return &line[..i],
            None => {}
        }
        previous = c;
    }
    line
}

fn unquote(value: &str) -> &str {
    let value = value.trim();
    for q in ['\'', '"'] {
        if let Some(inner) = value.strip_prefix(q).and_then(|rest| rest.strip_suffix(q)) {
            return inner;
        }
    }
    value
}

fn read_priority(entry: &Entry<'_>, front: &mut Frontmatter) {
    let raw = match entry.nested.first() {
        Some((_, nested)) if entry.value.is_empty() => *nested,
        _ => entry.value,
    };
    let parsed = unquote(raw)
        .parse::<i32>()
        .ok()
        .filter(|p| (MIN_PRIORITY..=MAX_PRIORITY).contains(p));
    match parsed {
        Some(priority) if entry.nested.is_empty() => front.priority = priority,
        _ => front
            .issues
            .push(LintIssue::new(LintCode::PriorityInvalid, entry.line).with("value", raw)),
    }
}

fn read_kind(entry: &Entry<'_>, front: &mut Frontmatter) {
    let raw = unquote(entry.value);
    match raw.parse::<WorkKind>() {
        Ok(kind) if entry.nested.is_empty() => front.kind = kind,
        _ => front
            .issues
            .push(LintIssue::new(LintCode::KindInvalid, entry.line).with("value", raw)),
    }
}

fn read_depends_on(entry: &Entry<'_>, slug: &str, front: &mut Frontmatter) {
    front.depends_on_line = Some(entry.line);
    let invalid = |line: u32, value: &str| {
        LintIssue::new(LintCode::DependsOnInvalid, line).with("value", value)
    };
    let mut items: Vec<(u32, &str)> = Vec::new();
    if entry.value.is_empty() {
        for (line, nested) in &entry.nested {
            match nested.strip_prefix('-') {
                Some(item) if item.is_empty() || item.starts_with([' ', '\t']) => {
                    items.push((*line, unquote(item)));
                }
                _ => front.issues.push(invalid(*line, nested)),
            }
        }
    } else if let Some(inner) = entry
        .value
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
    {
        items.extend(
            inner
                .split(',')
                .map(unquote)
                .filter(|item| !item.is_empty())
                .map(|item| (entry.line, item)),
        );
        front.issues.extend(
            entry
                .nested
                .iter()
                .map(|(line, nested)| invalid(*line, nested)),
        );
    } else {
        front.issues.push(invalid(entry.line, entry.value));
        return;
    }
    for (line, item) in items {
        if !valid_slug(item) {
            front.issues.push(invalid(line, item));
        } else if item == slug {
            front
                .issues
                .push(LintIssue::new(LintCode::DependsOnSelf, line).with("slug", item));
        } else if !front.depends_on.iter().any(|d| d == item) {
            front.depends_on.push(item.to_string());
        }
    }
}
