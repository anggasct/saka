//! Spec model: acceptance criteria, implementation steps, references, and
//! the decision section, extracted from one markdown spec as written.

use pulldown_cmark::{Event, HeadingLevel, LinkType, Options, Parser, Tag, TagEnd};

pub fn options() -> Options {
    Options::ENABLE_TABLES
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_YAML_STYLE_METADATA_BLOCKS
        | Options::ENABLE_WIKILINKS
}

#[derive(Debug, Default, serde::Serialize)]
pub struct Spec {
    pub title: Option<String>,
    pub frontmatter: bool,
    pub header_line: bool,
    pub sections: Vec<String>,
    pub acs: Vec<Ac>,
    pub steps: Vec<Step>,
    pub step_columns: Vec<String>,
    pub has_order_section: bool,
    pub step_form: Option<&'static str>,
    pub acs_outside_section: bool,
    pub links: Vec<(String, usize)>,
    pub approach: Option<String>,
    pub dor_checked: usize,
    pub dor_total: usize,
    pub verification_cmds: usize,
}

#[derive(Debug, serde::Serialize)]
pub struct Ac {
    pub id: String,
    pub checked: Option<bool>,
    pub form: &'static str,
    pub line: usize,
    pub section: String,
    pub text: String,
    #[serde(skip)]
    in_ac_section: bool,
}

#[derive(Debug, serde::Serialize)]
pub struct Step {
    pub id: String,
    pub line: usize,
    pub objective: String,
    pub done_when: String,
    pub ac_refs: Vec<String>,
}

struct Item {
    text: String,
    task: Option<bool>,
    line: usize,
    open: bool,
}

#[derive(Default)]
struct Table {
    header: Vec<String>,
    rows: Vec<(Vec<String>, usize)>,
    row: Vec<String>,
    row_line: usize,
    cell: Option<String>,
}

pub fn extract(text: &str) -> Spec {
    let lines = LineIndex::new(text);
    let mut spec = Spec {
        header_line: text.lines().take(12).any(|l| l.starts_with("> Created:")),
        ..Default::default()
    };
    let mut section = String::new();
    let mut approach_start: Option<usize> = None;
    let mut heading: Option<(HeadingLevel, String, usize, usize)> = None;
    let mut items: Vec<Item> = Vec::new();
    let mut table: Option<Table> = None;
    let mut in_code = false;
    let mut in_meta = false;
    let mut lists: Vec<Option<u64>> = Vec::new();

    for (event, range) in Parser::new_ext(text, options()).into_offset_iter() {
        let line = lines.line(range.start);
        let lower_section = section.to_lowercase();
        let in_verification = lower_section.starts_with("verification");
        let in_ac_section = lower_section.contains("acceptance criteria");
        let in_order = lower_section.starts_with("implementation order");
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                heading = Some((level, String::new(), line, range.start))
            }
            Event::End(TagEnd::Heading(_)) => {
                let Some((level, title, hline, start)) = heading.take() else {
                    continue;
                };
                let title = title.trim().to_string();
                if level == HeadingLevel::H1 && spec.title.is_none() {
                    spec.title = Some(title.clone());
                }
                if level == HeadingLevel::H2 {
                    if let Some(s) = approach_start.take() {
                        spec.approach = Some(text[s..start].to_string());
                    }
                    let lower = title.to_lowercase();
                    if lower.contains("selected")
                        || lower.contains("decision")
                        || lower.contains("approach")
                    {
                        approach_start = Some(range.end);
                    }
                    if lower.starts_with("implementation order") {
                        spec.has_order_section = true;
                    }
                    spec.sections.push(title.clone());
                    section = title.clone();
                }
                if let Some((id, rest)) = parse_ac(&title) {
                    let in_ac_section = section.to_lowercase().contains("acceptance criteria");
                    spec.acs.push(Ac {
                        id,
                        checked: None,
                        form: "heading",
                        line: hline,
                        section: section.clone(),
                        text: rest,
                        in_ac_section,
                    });
                }
            }
            Event::Start(Tag::MetadataBlock(_)) => {
                spec.frontmatter = true;
                in_meta = true;
            }
            Event::End(TagEnd::MetadataBlock(_)) => in_meta = false,
            Event::Start(Tag::CodeBlock(_)) => in_code = true,
            Event::End(TagEnd::CodeBlock) => in_code = false,
            Event::Start(Tag::Item) => items.push(Item {
                text: String::new(),
                task: None,
                line,
                open: true,
            }),
            Event::Start(Tag::List(start)) => {
                lists.push(start);
                if let Some(top) = items.last_mut() {
                    top.open = false;
                }
            }
            Event::End(TagEnd::List(_)) => {
                lists.pop();
            }
            Event::End(TagEnd::Paragraph) => {
                if let Some(top) = items.last_mut() {
                    top.open = false;
                }
            }
            Event::TaskListMarker(done) => {
                if let Some(top) = items.last_mut() {
                    top.task = Some(done);
                }
                if section.to_lowercase().starts_with("definition of ready") {
                    spec.dor_total += 1;
                    spec.dor_checked += usize::from(done);
                }
            }
            Event::End(TagEnd::Item) => {
                let Some(item) = items.pop() else { continue };
                if let Some((id, rest)) = parse_ac(item.text.trim()) {
                    let form = if item.task.is_some() { "task" } else { "list" };
                    spec.acs.push(Ac {
                        id,
                        checked: item.task,
                        form,
                        line: item.line,
                        section: section.clone(),
                        text: rest,
                        in_ac_section,
                    });
                } else if in_order
                    && items.is_empty()
                    && spec.step_form != Some("table")
                    && let Some(Some(first)) = lists.last()
                {
                    // An ordered list instead of the step table.
                    let n = *first as usize + spec.steps.len();
                    let text = item.text.trim().to_string();
                    spec.steps.push(Step {
                        id: n.to_string(),
                        line: item.line,
                        objective: text.clone(),
                        ac_refs: ac_refs(&text),
                        done_when: text,
                    });
                    spec.step_form = Some("list");
                }
            }
            Event::Start(Tag::Table(_)) => table = Some(Table::default()),
            Event::Start(Tag::TableRow) => {
                if let Some(t) = table.as_mut() {
                    t.row.clear();
                    t.row_line = line;
                }
            }
            Event::Start(Tag::TableCell) => {
                if let Some(t) = table.as_mut() {
                    t.cell = Some(String::new());
                }
            }
            Event::End(TagEnd::TableCell) => {
                if let Some(t) = table.as_mut() {
                    let c = t.cell.take().unwrap_or_default();
                    t.row.push(c.trim().to_string());
                }
            }
            Event::End(TagEnd::TableHead) => {
                if let Some(t) = table.as_mut() {
                    t.header = std::mem::take(&mut t.row);
                }
            }
            Event::End(TagEnd::TableRow) => {
                if let Some(t) = table.as_mut() {
                    let row = std::mem::take(&mut t.row);
                    t.rows.push((row, t.row_line));
                }
            }
            Event::End(TagEnd::Table) => {
                if let Some(t) = table.take() {
                    take_table(&mut spec, &section, t);
                }
            }
            Event::Start(Tag::Link {
                link_type,
                dest_url,
                ..
            }) => {
                let is_md_file = !dest_url.contains("://") && dest_url.ends_with(".md");
                if matches!(link_type, LinkType::WikiLink { .. }) || is_md_file {
                    // pulldown-cmark keeps the backslash of an escaped pipe in tables: [[a\|b]].
                    spec.links
                        .push((dest_url.trim_end_matches('\\').to_string(), line));
                }
            }
            Event::Text(t) if !in_meta => {
                if in_code {
                    if in_verification {
                        spec.verification_cmds +=
                            t.lines().filter(|l| !l.trim().is_empty()).count();
                    }
                } else {
                    push_text(&mut heading, &mut table, &mut items, &t);
                }
            }
            Event::Code(c) => {
                if in_verification {
                    spec.verification_cmds += 1;
                }
                push_text(&mut heading, &mut table, &mut items, &format!("`{c}`"));
            }
            Event::SoftBreak | Event::HardBreak => {
                push_text(&mut heading, &mut table, &mut items, " ")
            }
            _ => {}
        }
    }
    if let Some(s) = approach_start {
        spec.approach = Some(text[s..].to_string());
    }
    // AC IDs at the start of items elsewhere (Verification, DoD) are references.
    if spec.acs.iter().any(|a| a.in_ac_section) {
        spec.acs.retain(|a| a.in_ac_section);
    } else {
        spec.acs_outside_section = !spec.acs.is_empty();
    }
    spec
}

fn push_text(
    heading: &mut Option<(HeadingLevel, String, usize, usize)>,
    table: &mut Option<Table>,
    items: &mut [Item],
    s: &str,
) {
    if let Some((_, h, _, _)) = heading.as_mut() {
        h.push_str(s);
    } else if let Some(cell) = table.as_mut().and_then(|t| t.cell.as_mut()) {
        cell.push_str(s);
    } else if let Some(top) = items.last_mut()
        && top.open
    {
        top.text.push_str(s);
    }
}

fn take_table(spec: &mut Spec, section: &str, t: Table) {
    let cols: Vec<String> = t.header.iter().map(|h| h.to_lowercase()).collect();
    if section.to_lowercase().starts_with("implementation order") && spec.step_form != Some("table")
    {
        spec.steps.clear();
        let find = |name: &str| cols.iter().position(|c| c.contains(name));
        let (Some(step), Some(done)) = (find("step"), find("done when")) else {
            spec.step_columns = t.header;
            return;
        };
        let objective = find("objective");
        spec.step_form = Some("table");
        for (row, line) in t.rows {
            let cell = |i: usize| row.get(i).cloned().unwrap_or_default();
            let done_when = cell(done);
            spec.steps.push(Step {
                id: cell(step),
                line,
                objective: objective.map(cell).unwrap_or_default(),
                ac_refs: ac_refs(&done_when),
                done_when,
            });
        }
        spec.step_columns = t.header;
        return;
    }
    for (row, line) in t.rows {
        if let Some((id, rest)) = row.first().and_then(|c| parse_ac(c)) {
            let text = if rest.is_empty() {
                row.get(1).cloned().unwrap_or_default()
            } else {
                rest
            };
            let in_ac_section = section.to_lowercase().contains("acceptance criteria");
            spec.acs.push(Ac {
                id,
                checked: None,
                form: "table",
                line,
                section: section.to_string(),
                text,
                in_ac_section,
            });
        }
    }
}

/// `AC-01: text`, `AC-1 — text`, `AC-SEC-02 text` give the ID and the rest.
pub fn parse_ac(s: &str) -> Option<(String, String)> {
    let s = s.trim_start_matches(['*', '_', '`', ' ']);
    let tail = s.strip_prefix("AC-")?;
    let len = tail
        .char_indices()
        .take_while(|(i, c)| {
            c.is_ascii_digit()
                || c.is_ascii_uppercase()
                || (*c == '-' && tail[i + 1..].starts_with(|n: char| n.is_ascii_alphanumeric()))
        })
        .map(|(i, c)| i + c.len_utf8())
        .last()?;
    let id = format!("AC-{}", &tail[..len]);
    if !id[3..].chars().any(|c| c.is_ascii_digit()) {
        return None;
    }
    let rest = tail[len..]
        .trim_start_matches(['*', '_', '`', ' ', ':', '—', '–', '-', '.', ')'])
        .trim();
    Some((id, rest.to_string()))
}

pub fn canonical(id: &str) -> bool {
    id.strip_prefix("AC-")
        .is_some_and(|n| n.len() >= 2 && n.chars().all(|c| c.is_ascii_digit()))
}

/// AC IDs mentioned in a cell, with ranges like `AC-01–AC-04` or `AC-01..05` expanded.
pub fn ac_refs(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut rest = text;
    while let Some(pos) = rest.find("AC-") {
        let Some((id, _)) = parse_ac(&rest[pos..]) else {
            rest = &rest[pos + 3..];
            continue;
        };
        let after = &rest[pos + id.len()..];
        let sep = ["–", "—", "..", " to ", "-"]
            .iter()
            .find(|s| after.starts_with(**s));
        let mut consumed = pos + id.len();
        if let (Some(sep), Some(lo)) = (sep, num(&id)) {
            let next = &after[sep.len()..];
            let next = next.strip_prefix("AC-").unwrap_or(next);
            let digits: String = next.chars().take_while(char::is_ascii_digit).collect();
            if let Ok(hi) = digits.parse::<u32>()
                && hi > lo
                && hi - lo < 50
            {
                let width = id.len() - 3;
                out.extend((lo..=hi).map(|n| format!("AC-{n:0width$}")));
                consumed += sep.len() + (after.len() - sep.len() - next.len()) + digits.len();
                rest = &rest[consumed..];
                continue;
            }
        }
        out.push(id);
        rest = &rest[consumed..];
    }
    out.dedup();
    out
}

fn num(id: &str) -> Option<u32> {
    id.strip_prefix("AC-")?.parse().ok()
}

struct LineIndex(Vec<usize>);

impl LineIndex {
    fn new(text: &str) -> Self {
        let mut starts = vec![0];
        starts.extend(text.match_indices('\n').map(|(i, _)| i + 1));
        LineIndex(starts)
    }

    fn line(&self, offset: usize) -> usize {
        self.0.partition_point(|&s| s <= offset)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ac_ids() {
        assert_eq!(
            parse_ac("AC-01: does x").unwrap(),
            ("AC-01".into(), "does x".into())
        );
        assert_eq!(parse_ac("AC-1 — y").unwrap().0, "AC-1");
        assert_eq!(parse_ac("AC-SEC-02 z").unwrap().0, "AC-SEC-02");
        assert!(parse_ac("ACME").is_none());
        assert!(
            canonical("AC-01") && canonical("AC-123") && !canonical("AC-1") && !canonical("AC-T01")
        );
    }

    #[test]
    fn refs_and_ranges() {
        assert_eq!(ac_refs("AC-01, AC-03 pass"), vec!["AC-01", "AC-03"]);
        assert_eq!(
            ac_refs("AC-01–AC-03 green"),
            vec!["AC-01", "AC-02", "AC-03"]
        );
        assert_eq!(ac_refs("AC-04..06"), vec!["AC-04", "AC-05", "AC-06"]);
        assert_eq!(ac_refs("AC-02 to AC-03"), vec!["AC-02", "AC-03"]);
        assert!(ac_refs("all checks").is_empty());
    }

    #[test]
    fn extracts_spec() {
        let md = "# T\n\n> Created: 2026-01-01 | Feature: [[features/feat-x]]\n\n## Selected Technical Approach\n\n- **Selected approach:** B\n\n## Definition of Ready\n\n- [x] a\n- [ ] b\n\n## Acceptance Criteria\n\n- [ ] **AC-01**: first `go test ./x`\n- [x] AC-02 — second\n  - detail AC-09\n\n## Verification\n\n- `make check`\n\n## Implementation order\n\n| Step | Objective | Done when |\n|---|---|---|\n| 1 | api | AC-01 pass |\n| 2 | ui | AC-02 |\n";
        let s = extract(md);
        assert_eq!(s.title.as_deref(), Some("T"));
        assert!(s.header_line);
        assert_eq!(
            s.acs.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(),
            ["AC-01", "AC-02"]
        );
        assert_eq!(s.acs[0].checked, Some(false));
        assert_eq!(s.acs[1].text, "second");
        assert_eq!((s.dor_checked, s.dor_total), (1, 2));
        assert_eq!(s.steps.len(), 2);
        assert_eq!(s.steps[1].ac_refs, ["AC-02"]);
        assert_eq!(s.verification_cmds, 1);
        assert!(s.approach.unwrap().contains("Selected approach:** B"));
        assert_eq!(s.links, vec![("features/feat-x".to_string(), 3)]);
    }
}
