use std::collections::HashSet;

use comrak::nodes::{AstNode, NodeValue};

use crate::lint::{LintCode, LintIssue};
use crate::model::{Decision, DecisionOption};
use crate::parse::{inline_text, start_line};

pub const MAX_DECISIONS: usize = 20;

const MAX_HEADING_CHARS: usize = 80;
const MAX_OPTION_CHARS: usize = 500;
const OPTION_COUNT: std::ops::RangeInclusive<usize> = 2..=3;
const RECOMMENDED: &str = "(recommended)";

pub(crate) struct DecisionReader<'a, 's> {
    source: Vec<&'s str>,
    ids: HashSet<String>,
    full: bool,
    open: Option<Open<'a>>,
    decisions: Vec<Decision>,
}

struct Open<'a> {
    id: String,
    question: String,
    line: u32,
    list: Option<&'a AstNode<'a>>,
}

impl<'a, 's> DecisionReader<'a, 's> {
    pub(crate) fn new(body: &'s str) -> DecisionReader<'a, 's> {
        DecisionReader {
            source: source_lines(body),
            ids: HashSet::new(),
            full: false,
            open: None,
            decisions: Vec::new(),
        }
    }

    pub(crate) fn heading(&mut self, node: &'a AstNode<'a>, issues: &mut Vec<LintIssue>) {
        self.close(issues);
        if self.full {
            return;
        }
        let line = start_line(node);
        let text = inline_text(node).trim().to_string();
        let Some((id, question)) = decision_heading(&text) else {
            issues.push(
                LintIssue::new(LintCode::DecisionIdInvalid, line)
                    .with("heading", truncate(&text, MAX_HEADING_CHARS)),
            );
            return;
        };
        if self.ids.contains(&id) {
            issues.push(LintIssue::new(LintCode::DecisionIdDuplicate, line).with("id", id));
            return;
        }
        if self.decisions.len() == MAX_DECISIONS {
            issues.push(
                LintIssue::new(LintCode::DecisionLimitExceeded, line).with("max", MAX_DECISIONS),
            );
            self.full = true;
            return;
        }
        self.ids.insert(id.clone());
        self.open = Some(Open {
            id,
            question,
            line,
            list: None,
        });
    }

    pub(crate) fn block(&mut self, node: &'a AstNode<'a>) {
        let is_list = matches!(node.data.borrow().value, NodeValue::List(_));
        if let Some(open) = self.open.as_mut().filter(|o| is_list && o.list.is_none()) {
            open.list = Some(node);
        }
    }

    pub(crate) fn close(&mut self, issues: &mut Vec<LintIssue>) {
        let Some(open) = self.open.take() else {
            return;
        };
        let mut decision = Decision {
            id: open.id,
            question: open.question,
            line: open.line,
            options: Vec::new(),
            chosen: None,
        };
        let invalid = |reason: &str, line: u32, id: &str| {
            LintIssue::new(LintCode::DecisionOptionsInvalid, line)
                .with("id", id)
                .with("reason", reason)
        };
        let Some(list) = open.list else {
            issues.push(invalid("missing", decision.line, &decision.id));
            self.decisions.push(decision);
            return;
        };
        let items: Vec<&'a AstNode<'a>> = list.children().collect();
        let mut broken = false;
        if !OPTION_COUNT.contains(&items.len()) {
            issues.push(invalid("count", decision.line, &decision.id).with("count", items.len()));
            broken = true;
        }
        for (n, item) in items.iter().enumerate() {
            match self.option(item) {
                Some(option) => decision.options.push(option),
                None => {
                    issues.push(
                        invalid("not_task", start_line(item), &decision.id).with("option", n + 1),
                    );
                    broken = true;
                }
            }
        }
        if !broken {
            check_recommendation(&decision, issues);
            let checked: Vec<usize> = decision
                .options
                .iter()
                .enumerate()
                .filter(|(_, o)| o.chosen)
                .map(|(n, _)| n + 1)
                .collect();
            match checked.as_slice() {
                [] => issues.push(
                    LintIssue::new(LintCode::DecisionUnanswered, decision.line)
                        .with("id", decision.id.clone()),
                ),
                [one] => decision.chosen = u32::try_from(*one).ok(),
                _ => issues.push(
                    LintIssue::new(LintCode::DecisionAnswerMultiple, decision.line)
                        .with("id", decision.id.clone()),
                ),
            }
        }
        self.decisions.push(decision);
    }

    pub(crate) fn finish(mut self, issues: &mut Vec<LintIssue>) -> Vec<Decision> {
        self.close(issues);
        self.decisions
    }

    fn option(&self, item: &'a AstNode<'a>) -> Option<DecisionOption> {
        let paragraph = item
            .children()
            .find(|c| matches!(c.data.borrow().value, NodeValue::Paragraph))?;
        let pos = paragraph.data.borrow().sourcepos;
        let first = self
            .line(pos.start.line)?
            .get(pos.start.column.checked_sub(1)?..)?;
        let chosen = match first.as_bytes() {
            [b'[', b' ', b']', b' ' | b'\t', ..] => false,
            [b'[', b'x' | b'X', b']', b' ' | b'\t', ..] => true,
            _ => return None,
        };
        let mut text = first.get(4..)?.to_string();
        for n in pos.start.line + 1..=pos.end.line {
            text.push('\n');
            text.push_str(self.line(n).unwrap_or_default().trim_start());
        }
        let text = text.trim();
        Some(DecisionOption {
            text: truncate(text, MAX_OPTION_CHARS),
            line: start_line(item),
            recommended: text.to_lowercase().contains(RECOMMENDED),
            chosen,
        })
    }

    fn line(&self, number: usize) -> Option<&'s str> {
        self.source.get(number.checked_sub(1)?).copied()
    }
}

fn check_recommendation(decision: &Decision, issues: &mut Vec<LintIssue>) {
    let issue = |reason: &str, option: usize, line: u32| {
        LintIssue::new(LintCode::DecisionRecommendationInvalid, line)
            .with("id", decision.id.clone())
            .with("reason", reason)
            .with("option", option)
    };
    for (n, option) in decision.options.iter().enumerate() {
        match (n, option.recommended) {
            (0, false) => issues.push(issue("first_missing", 1, option.line)),
            (0, true) | (_, false) => {}
            (_, true) => issues.push(issue("other_marked", n + 1, option.line)),
        }
    }
}

fn decision_heading(text: &str) -> Option<(String, String)> {
    let rest = text.strip_prefix('Q')?;
    let digits = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    let (number, rest) = rest.split_at(digits);
    if !(1..=2).contains(&number.len()) || number.starts_with('0') {
        return None;
    }
    let rest = rest.strip_prefix(':')?;
    let question = rest.trim_start_matches(' ');
    if question.len() == rest.len()
        || question.is_empty()
        || question.starts_with(char::is_whitespace)
    {
        return None;
    }
    Some((format!("Q{number}"), question.trim().to_string()))
}

fn truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut cut: String = text.chars().take(max_chars - 1).collect();
    cut.push('…');
    cut
}

// Line numbers must match comrak's, which ends a line at "\r\n", "\n", or a lone "\r".
fn source_lines(body: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let bytes = body.as_bytes();
    let mut start = 0;
    let mut i = 0;
    while let Some(&b) = bytes.get(i) {
        match b {
            b'\n' | b'\r' => {
                lines.push(&body[start..i]);
                i += if b == b'\r' && bytes.get(i + 1) == Some(&b'\n') {
                    2
                } else {
                    1
                };
                start = i;
            }
            _ => i += 1,
        }
    }
    lines.push(&body[start..]);
    lines
}
