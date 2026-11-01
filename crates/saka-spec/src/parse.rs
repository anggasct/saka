use std::collections::HashSet;

use comrak::nodes::{AstNode, NodeValue};
use comrak::{Arena, Options, parse_document};

use crate::lint::{LintCode, LintIssue};
use crate::model::{AcceptanceCriterion, SpecModel, Step};

pub const PARSER_VERSION: u32 = 1;
pub const MAX_SPEC_BYTES: usize = 256 * 1024;
pub const MAX_STEPS: usize = 50;

const REQUIRED_COLUMNS: [&str; 4] = ["step", "work", "ac", "done when"];
const UNSUPPORTED_COLUMNS: [&str; 2] = ["repo", "after"];

#[derive(Debug, Clone, PartialEq)]
pub struct Parsed {
    pub model: Option<SpecModel>,
    pub issues: Vec<LintIssue>,
}

impl Parsed {
    pub fn is_ready(&self) -> bool {
        self.model.is_some() && self.issues.is_empty()
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
    if !has_spec_marker(text) {
        return fail(LintIssue::new(LintCode::NotASpec, 1));
    }

    let mut issues = Vec::new();
    let slug = slug_of(file_name);
    if !valid_slug(&slug) {
        issues.push(LintIssue::new(LintCode::SlugInvalid, 1).with("slug", slug.clone()));
    }

    let arena = Arena::new();
    let mut options = Options::default();
    options.extension.table = true;
    options.extension.front_matter_delimiter = Some("---".to_string());
    let root = parse_document(&arena, text, &options);

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
            _ => {}
        }
    }

    if doc.title.as_deref().is_none_or(str::is_empty) {
        issues.push(LintIssue::new(LintCode::TitleMissing, 1));
    }
    if !doc.ac_section {
        issues.push(LintIssue::new(LintCode::AcSectionMissing, 1));
    }
    let steps = match &doc.table {
        None => {
            issues.push(LintIssue::new(
                LintCode::StepsMissing,
                doc.steps_heading.unwrap_or(1),
            ));
            Vec::new()
        }
        // Without a criteria section every reference would be unknown; report the section once.
        Some(table) => steps_from_table(table, doc.ac_section.then_some(&doc.ac_ids), &mut issues),
    };

    issues.sort_by_key(|i| i.line);
    Parsed {
        model: Some(SpecModel {
            slug,
            title: doc.title.unwrap_or_default(),
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

fn has_spec_marker(text: &str) -> bool {
    let mut lines = text.lines();
    if lines.next().map(str::trim_end) != Some("---") {
        return false;
    }
    for line in lines {
        let line = line.trim_end();
        if line == "---" || line == "..." {
            return false;
        }
        if let Some(value) = line.strip_prefix("saka:") {
            let value = value.trim().trim_matches(|c| c == '"' || c == '\'');
            return value == "spec";
        }
    }
    false
}

fn slug_of(file_name: &str) -> String {
    let name = file_name.rsplit(['/', '\\']).next().unwrap_or(file_name);
    name.strip_suffix(".md").unwrap_or(name).to_string()
}

fn valid_slug(slug: &str) -> bool {
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

fn read_table<'a>(table: &'a AstNode<'a>) -> Table {
    let mut header_line = start_line(table);
    let mut columns = Vec::new();
    let mut rows = Vec::new();
    for row in table.children() {
        let is_header = matches!(row.data.borrow().value, NodeValue::TableRow(true));
        let cells: Vec<Cell> = row
            .children()
            .map(|cell| {
                let mut code = Vec::new();
                for node in cell.descendants() {
                    if let NodeValue::Code(c) = &node.data.borrow().value {
                        code.push(c.literal.trim().to_string());
                    }
                }
                Cell {
                    text: inline_text(cell).trim().to_string(),
                    code,
                }
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
    let empty = Cell {
        text: String::new(),
        code: Vec::new(),
    };
    let mut steps = Vec::new();
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
        let ac: Vec<String> = cell(ac_col)
            .text
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
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
        steps.push(Step {
            id: expected,
            index: u32::try_from(position).unwrap_or(u32::MAX),
            work: cell(work_col).text.clone(),
            ac,
            commands,
            line: row.line,
        });
    }
    steps
}
