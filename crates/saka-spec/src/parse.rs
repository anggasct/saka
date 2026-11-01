use std::collections::{HashMap, HashSet};

use comrak::nodes::{AstNode, NodeValue};
use comrak::{Arena, Options, parse_document};

use crate::frontmatter;
use crate::graph::cycles;
use crate::lint::{LintCode, LintIssue};
use crate::model::{AcceptanceCriterion, SpecModel, Step};

pub const PARSER_VERSION: u32 = 2;
pub const MAX_SPEC_BYTES: usize = 256 * 1024;
pub const MAX_STEPS: usize = 50;

const REQUIRED_COLUMNS: [&str; 4] = ["step", "work", "ac", "done when"];
const UNSUPPORTED_COLUMNS: [&str; 1] = ["repo"];
const AFTER_COLUMN: &str = "after";

#[derive(Debug, Clone, PartialEq)]
pub struct Parsed {
    pub model: Option<SpecModel>,
    pub issues: Vec<LintIssue>,
}

impl Parsed {
    pub fn is_ready(&self) -> bool {
        self.model.is_some() && !self.issues.iter().any(LintIssue::is_error)
    }
}

pub fn parse(file_name: &str, bytes: &[u8]) -> Parsed {
    let fail = |issue: LintIssue| Parsed {
        model: None,
        issues: vec![issue],
    };
    if bytes.len() > MAX_SPEC_BYTES {
        return fail(
            LintIssue::new(LintCode::FileTooLarge, 1)
                .with("bytes", bytes.len())
                .with("max_bytes", MAX_SPEC_BYTES),
        );
    }
    let text = match std::str::from_utf8(bytes) {
        Ok(text) => text,
        Err(err) => {
            return fail(LintIssue::new(
                LintCode::InvalidUtf8,
                line_at(bytes, err.valid_up_to()),
            ));
        }
    };
    let slug = slug_of(file_name);
    let Some(front) = frontmatter::read(text, &slug).filter(|f| f.is_spec) else {
        return fail(LintIssue::new(LintCode::NotASpec, 1));
    };

    let mut issues = front.issues;
    if !valid_slug(&slug) {
        issues.push(LintIssue::new(LintCode::SlugInvalid, 1).with("slug", slug.clone()));
    }

    // Blank lines stand in for the frontmatter so source positions stay file lines.
    let blank = usize::try_from(front.end_line).unwrap_or_default();
    let body = format!("{}{}", "\n".repeat(blank), &text[front.body_offset..]);
    let arena = Arena::new();
    let mut options = Options::default();
    options.extension.table = true;
    let root = parse_document(&arena, &body, &options);

    let mut doc = Document::default();
    let mut section = Section::Other;
    for node in root.children() {
        let line = start_line(node);
        let heading = match &node.data.borrow().value {
            NodeValue::Heading(h) => Some(h.level),
            _ => None,
        };
        if let Some(level) = heading {
            let title = inline_text(node).trim().to_string();
            if level == 1 && doc.title.is_none() {
                doc.title = Some(title.clone());
            }
            if level == 2 {
                section = match title.to_lowercase().as_str() {
                    "acceptance criteria" => {
                        doc.ac_section = true;
                        Section::Ac
                    }
                    "steps" => {
                        doc.steps_heading.get_or_insert(line);
                        Section::Steps
                    }
                    "done when" if doc.done_when.is_none() => {
                        doc.done_when = Some(DoneWhen { line, items: None });
                        Section::DoneWhen
                    }
                    _ => Section::Other,
                };
            }
            continue;
        }
        let is_list = matches!(node.data.borrow().value, NodeValue::List(_));
        let is_table = matches!(node.data.borrow().value, NodeValue::Table(_));
        match section {
            Section::Ac if is_list => collect_acs(node, &mut doc, &mut issues),
            Section::Steps if is_table && doc.table.is_none() => doc.table = Some(read_table(node)),
            Section::DoneWhen if is_list => {
                if let Some(done_when) = doc.done_when.as_mut().filter(|d| d.items.is_none()) {
                    done_when.items = Some(read_done_when(node));
                }
            }
            _ => {}
        }
    }

    let title = doc.title.clone().unwrap_or_default();
    if title.is_empty() {
        issues.push(LintIssue::new(LintCode::TitleMissing, 1));
    }
    if !doc.ac_section {
        issues.push(LintIssue::new(LintCode::AcSectionMissing, 1));
    }
    let steps = match (&doc.table, &doc.done_when) {
        (Some(table), done_when) => {
            if let Some(done_when) = done_when {
                issues.push(LintIssue::new(LintCode::DoneWhenWithSteps, done_when.line));
            }
            // Without a criteria section every reference would be unknown; report the section once.
            steps_from_table(table, doc.ac_section.then_some(&doc.ac_ids), &mut issues)
        }
        (None, Some(done_when)) => one_step(done_when, &title, &doc, &mut issues),
        (None, None) => {
            issues.push(LintIssue::new(
                LintCode::StepsMissing,
                doc.steps_heading.unwrap_or(1),
            ));
            Vec::new()
        }
    };
    check_commands(&steps, &mut issues);
    if !steps.is_empty() {
        check_coverage(&doc.acs, &steps, &mut issues);
    }

    issues.sort_by_key(|i| i.line);
    Parsed {
        model: Some(SpecModel {
            slug,
            title,
            priority: front.priority,
            depends_on: front.depends_on,
            acceptance_criteria: doc.acs,
            steps,
        }),
        issues,
    }
}

#[derive(Clone, Copy)]
enum Section {
    Ac,
    Steps,
    DoneWhen,
    Other,
}

#[derive(Default)]
struct Document {
    title: Option<String>,
    ac_section: bool,
    acs: Vec<AcceptanceCriterion>,
    ac_ids: HashSet<String>,
    steps_heading: Option<u32>,
    table: Option<Table>,
    done_when: Option<DoneWhen>,
}

struct DoneWhen {
    line: u32,
    items: Option<Vec<DoneWhenItem>>,
}

struct DoneWhenItem {
    line: u32,
    code: Vec<String>,
}

struct Table {
    header_line: u32,
    columns: Vec<String>,
    rows: Vec<Row>,
}

struct Row {
    line: u32,
    cells: Vec<Cell>,
}

struct Cell {
    text: String,
    code: Vec<String>,
}

fn line_at(bytes: &[u8], offset: usize) -> u32 {
    let newlines = bytes[..offset].iter().filter(|&&b| b == b'\n').count();
    u32::try_from(newlines + 1).unwrap_or(u32::MAX)
}

fn start_line<'a>(node: &'a AstNode<'a>) -> u32 {
    u32::try_from(node.data.borrow().sourcepos.start.line).unwrap_or(u32::MAX)
}

fn slug_of(file_name: &str) -> String {
    let name = file_name.rsplit(['/', '\\']).next().unwrap_or(file_name);
    name.strip_suffix(".md").unwrap_or(name).to_string()
}

pub(crate) fn valid_slug(slug: &str) -> bool {
    let mut chars = slug.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    slug.len() <= 63
        && (first.is_ascii_lowercase() || first.is_ascii_digit())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn valid_ac_id(id: &str) -> bool {
    id.strip_prefix("AC-")
        .is_some_and(|n| (2..=3).contains(&n.len()) && n.chars().all(|c| c.is_ascii_digit()))
}

fn inline_text<'a>(node: &'a AstNode<'a>) -> String {
    let mut out = String::new();
    push_text(node, &mut out);
    out
}

fn push_text<'a>(node: &'a AstNode<'a>, out: &mut String) {
    for child in node.children() {
        match &child.data.borrow().value {
            NodeValue::Text(t) => out.push_str(t),
            NodeValue::Code(c) => {
                out.push('`');
                out.push_str(&c.literal);
                out.push('`');
            }
            NodeValue::SoftBreak | NodeValue::LineBreak => out.push(' '),
            _ => push_text(child, out),
        }
    }
}

fn code_spans<'a>(node: &'a AstNode<'a>) -> Vec<String> {
    node.descendants()
        .filter_map(|n| match &n.data.borrow().value {
            NodeValue::Code(c) => Some(c.literal.trim().to_string()),
            _ => None,
        })
        .collect()
}

fn collect_acs<'a>(list: &'a AstNode<'a>, doc: &mut Document, issues: &mut Vec<LintIssue>) {
    for item in list.children() {
        let line = start_line(item);
        let Some(paragraph) = item
            .children()
            .find(|c| matches!(c.data.borrow().value, NodeValue::Paragraph))
        else {
            continue;
        };
        let Some(first) = paragraph.first_child() else {
            continue;
        };
        if !matches!(first.data.borrow().value, NodeValue::Strong) {
            continue;
        }
        let id = inline_text(first).trim().to_string();
        if !id.starts_with("AC-") {
            continue;
        }
        if !valid_ac_id(&id) {
            issues.push(LintIssue::new(LintCode::AcIdInvalid, line).with("id", id));
            continue;
        }
        if !doc.ac_ids.insert(id.clone()) {
            issues.push(LintIssue::new(LintCode::AcIdDuplicate, line).with("id", id));
            continue;
        }
        let mut rest = String::new();
        let mut sibling = first.next_sibling();
        while let Some(node) = sibling {
            let mut piece = String::new();
            match &node.data.borrow().value {
                NodeValue::Text(t) => piece.push_str(t),
                NodeValue::Code(c) => piece = format!("`{}`", c.literal),
                NodeValue::SoftBreak | NodeValue::LineBreak => piece.push(' '),
                _ => push_text(node, &mut piece),
            }
            rest.push_str(&piece);
            sibling = node.next_sibling();
        }
        let text = rest.trim_start().trim_start_matches(':').trim().to_string();
        doc.acs.push(AcceptanceCriterion { id, text, line });
    }
}

fn read_done_when<'a>(list: &'a AstNode<'a>) -> Vec<DoneWhenItem> {
    list.children()
        .map(|item| DoneWhenItem {
            line: start_line(item),
            code: item
                .children()
                .filter(|c| matches!(c.data.borrow().value, NodeValue::Paragraph))
                .flat_map(code_spans)
                .collect(),
        })
        .collect()
}

fn one_step(
    done_when: &DoneWhen,
    title: &str,
    doc: &Document,
    issues: &mut Vec<LintIssue>,
) -> Vec<Step> {
    let items = done_when.items.as_deref().unwrap_or_default();
    let mut commands = Vec::new();
    for (n, item) in items.iter().enumerate() {
        match item.code.as_slice() {
            [command] if !command.is_empty() => commands.push(command.clone()),
            _ => issues
                .push(LintIssue::new(LintCode::DoneWhenItemInvalid, item.line).with("item", n + 1)),
        }
    }
    if items.is_empty() {
        issues
            .push(LintIssue::new(LintCode::StepCommandMissing, done_when.line).with("step", "S1"));
    }
    let ac: Vec<String> = doc.acs.iter().map(|a| a.id.clone()).collect();
    if ac.is_empty() && doc.ac_section {
        issues.push(LintIssue::new(LintCode::StepAcEmpty, done_when.line).with("step", "S1"));
    }
    vec![Step {
        id: "S1".to_string(),
        index: 1,
        work: title.to_string(),
        ac,
        commands,
        after: Vec::new(),
        line: done_when.line,
    }]
}

fn read_table<'a>(table: &'a AstNode<'a>) -> Table {
    let mut header_line = start_line(table);
    let mut columns = Vec::new();
    let mut rows = Vec::new();
    for row in table.children() {
        let is_header = matches!(row.data.borrow().value, NodeValue::TableRow(true));
        let cells: Vec<Cell> = row
            .children()
            .map(|cell| Cell {
                text: inline_text(cell).trim().to_string(),
                code: code_spans(cell),
            })
            .collect();
        if is_header {
            header_line = start_line(row);
            columns = cells.into_iter().map(|c| c.text.to_lowercase()).collect();
        } else {
            rows.push(Row {
                line: start_line(row),
                cells,
            });
        }
    }
    Table {
        header_line,
        columns,
        rows,
    }
}

fn steps_from_table(
    table: &Table,
    ac_ids: Option<&HashSet<String>>,
    issues: &mut Vec<LintIssue>,
) -> Vec<Step> {
    let column = |name: &str| table.columns.iter().position(|c| c == name);
    let mut broken = false;
    for name in UNSUPPORTED_COLUMNS {
        if column(name).is_some() {
            issues.push(
                LintIssue::new(LintCode::StepsColumnUnsupported, table.header_line)
                    .with("column", name),
            );
            broken = true;
        }
    }
    let mut index = [0usize; 4];
    for (slot, name) in REQUIRED_COLUMNS.iter().enumerate() {
        match column(name) {
            Some(i) => index[slot] = i,
            None => {
                issues.push(
                    LintIssue::new(LintCode::StepsColumnMissing, table.header_line)
                        .with("column", *name),
                );
                broken = true;
            }
        }
    }
    if broken {
        return Vec::new();
    }
    if table.rows.is_empty() {
        issues.push(LintIssue::new(LintCode::StepsMissing, table.header_line));
        return Vec::new();
    }
    let [step_col, work_col, ac_col, done_col] = index;
    let after_col = column(AFTER_COLUMN);
    let empty = Cell {
        text: String::new(),
        code: Vec::new(),
    };
    let mut steps = Vec::new();
    let mut written_after = Vec::new();
    for (i, row) in table.rows.iter().enumerate() {
        let position = i + 1;
        if position > MAX_STEPS {
            issues.push(
                LintIssue::new(LintCode::StepLimitExceeded, row.line).with("max_steps", MAX_STEPS),
            );
            break;
        }
        let cell = |c: usize| row.cells.get(c).unwrap_or(&empty);
        let expected = format!("S{position}");
        let found = cell(step_col).text.clone();
        if found != expected {
            issues.push(
                LintIssue::new(LintCode::StepIdInvalid, row.line)
                    .with("expected", expected.clone())
                    .with("found", found),
            );
        }
        let work = cell(work_col).text.clone();
        if work.is_empty() {
            issues.push(
                LintIssue::new(LintCode::StepWorkEmpty, row.line).with("step", expected.clone()),
            );
        }
        let ac = split_ids(&cell(ac_col).text);
        if ac.is_empty() {
            issues.push(
                LintIssue::new(LintCode::StepAcEmpty, row.line).with("step", expected.clone()),
            );
        }
        for id in ac
            .iter()
            .filter(|id| ac_ids.is_some_and(|known| !known.contains(*id)))
        {
            issues.push(
                LintIssue::new(LintCode::StepAcUnknown, row.line)
                    .with("step", expected.clone())
                    .with("ac", id.clone()),
            );
        }
        let commands: Vec<String> = cell(done_col)
            .code
            .iter()
            .filter(|c| !c.is_empty())
            .cloned()
            .collect();
        if commands.is_empty() {
            issues.push(
                LintIssue::new(LintCode::StepCommandMissing, row.line)
                    .with("step", expected.clone()),
            );
        }
        written_after.push(after_col.map(|c| split_ids(&cell(c).text)));
        let after = match position {
            1 => Vec::new(),
            n => vec![format!("S{}", n - 1)],
        };
        steps.push(Step {
            id: expected,
            index: u32::try_from(position).unwrap_or(u32::MAX),
            work,
            ac,
            commands,
            after,
            line: row.line,
        });
    }
    if after_col.is_some() {
        apply_after(&mut steps, &written_after, issues);
    }
    steps
}

fn split_ids(text: &str) -> Vec<String> {
    let mut ids: Vec<String> = Vec::new();
    for id in text.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        if !ids.iter().any(|known| known == id) {
            ids.push(id.to_string());
        }
    }
    ids
}

fn apply_after(steps: &mut [Step], written: &[Option<Vec<String>>], issues: &mut Vec<LintIssue>) {
    let position: HashMap<String, usize> = steps
        .iter()
        .enumerate()
        .map(|(i, s)| (s.id.clone(), i))
        .collect();
    for (step, written) in steps.iter_mut().zip(written) {
        let written = written.as_deref().unwrap_or_default();
        if written.is_empty() && step.index > 1 {
            issues.push(
                LintIssue::new(LintCode::StepParallelStart, step.line)
                    .with("step", step.id.clone()),
            );
        }
        step.after.clear();
        for id in written {
            if position.contains_key(id) {
                step.after.push(id.clone());
            } else {
                issues.push(
                    LintIssue::new(LintCode::StepAfterUnknown, step.line)
                        .with("step", step.id.clone())
                        .with("after", id.clone()),
                );
            }
        }
    }
    let edges: Vec<Vec<usize>> = steps
        .iter()
        .map(|s| {
            s.after
                .iter()
                .filter_map(|id| position.get(id).copied())
                .collect()
        })
        .collect();
    for cycle in cycles(&edges) {
        let first = cycle.first().copied().unwrap_or_default();
        let ids: Vec<String> = cycle.iter().map(|&i| steps[i].id.clone()).collect();
        issues.push(LintIssue::new(LintCode::StepAfterCycle, steps[first].line).with("steps", ids));
    }
}

fn check_commands(steps: &[Step], issues: &mut Vec<LintIssue>) {
    for step in steps {
        let mut seen = HashSet::new();
        let mut reported = HashSet::new();
        for command in &step.commands {
            if !seen.insert(command) && reported.insert(command) {
                issues.push(
                    LintIssue::new(LintCode::StepCommandDuplicate, step.line)
                        .with("step", step.id.clone())
                        .with("command", command.clone()),
                );
            }
        }
    }
}

fn check_coverage(acs: &[AcceptanceCriterion], steps: &[Step], issues: &mut Vec<LintIssue>) {
    let covered: HashSet<&String> = steps.iter().flat_map(|s| &s.ac).collect();
    for ac in acs.iter().filter(|a| !covered.contains(&a.id)) {
        issues.push(LintIssue::new(LintCode::AcUncovered, ac.line).with("ac", ac.id.clone()));
    }
}
