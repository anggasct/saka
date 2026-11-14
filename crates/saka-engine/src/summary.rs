use std::fmt::Write as _;

use saka_core::redact::Redactor;

pub(crate) const START: &str = "<!-- saka-summary -->";
pub(crate) const END: &str = "<!-- /saka-summary -->";
const MAX_BYTES: usize = 8 * 1024;
const HISTORY_MAX_BYTES: usize = 3 * MAX_BYTES;
const CUT_NOTE: &str = "… (summary cut at 8 KiB)";
const NONE_LINE: &str = "No summary from the agent.";

pub(crate) struct Round {
    pub label: String,
    pub text: Option<String>,
}

type Fence = Option<(char, usize)>;

fn fence_marker(line: &str) -> Option<(char, usize)> {
    let text = line.trim_start_matches(' ');
    if line.len() - text.len() > 3 {
        return None;
    }
    let mark = text.chars().next().filter(|c| matches!(c, '`' | '~'))?;
    let length = text.chars().take_while(|c| *c == mark).count();
    (length >= 3).then_some((mark, length))
}

fn advance(open: &mut Fence, line: &str) -> bool {
    let Some((mark, length)) = fence_marker(line) else {
        return false;
    };
    match *open {
        None => *open = Some((mark, length)),
        Some((opened, size))
            if opened == mark && length >= size && line.trim().chars().all(|c| c == mark) =>
        {
            *open = None;
        }
        Some(_) => {}
    }
    true
}

pub(crate) fn extract(result_text: &str) -> Option<String> {
    let mut open: Fence = None;
    let mut found = false;
    let mut section = Vec::new();
    for line in result_text.lines() {
        let inside = open.is_some();
        let on_fence = advance(&mut open, line);
        let plain = !inside && !on_fence;
        if !found {
            found = plain && line.trim_end().eq_ignore_ascii_case("## summary");
            continue;
        }
        if plain && (line.starts_with("# ") || line.starts_with("## ")) {
            break;
        }
        section.push(line);
    }
    let text = section.join("\n");
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

fn strip_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("<!--") {
        out.push_str(&rest[..at]);
        let after = &rest[at + 4..];
        rest = after.find("-->").map_or(after, |end| &after[end + 3..]);
    }
    out.push_str(rest);
    out
}

fn code_span_end(rest: &str, ticks: usize) -> Option<usize> {
    let mut at = 0;
    while let Some(first) = rest[at..].chars().next() {
        let run = rest[at..].chars().take_while(|c| *c == '`').count();
        if run == ticks {
            return Some(at + run);
        }
        at += if run == 0 { first.len_utf8() } else { run };
    }
    None
}

fn strip_tags(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut at = 0;
    while at < line.len() {
        let rest = &line[at..];
        let Some(first) = rest.chars().next() else {
            break;
        };
        if first == '`' {
            let ticks = rest.chars().take_while(|c| *c == '`').count();
            let end = code_span_end(&rest[ticks..], ticks).map_or(ticks, |e| ticks + e);
            out.push_str(&rest[..end]);
            at += end;
        } else if first == '<'
            && rest[1..]
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic() || matches!(c, '/' | '!' | '?'))
        {
            at += rest.find('>').map_or(rest.len(), |end| end + 1);
        } else {
            out.push(first);
            at += first.len_utf8();
        }
    }
    out
}

fn strip_html_outside_code(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut open: Fence = None;
    for raw in text.split_inclusive('\n') {
        let line = raw.trim_end_matches('\n');
        let inside = open.is_some();
        let on_fence = advance(&mut open, line);
        if inside || on_fence {
            out.push_str(raw);
        } else {
            out.push_str(&strip_tags(line));
            out.push_str(&raw[line.len()..]);
        }
    }
    out
}

fn strip_html(text: &str) -> String {
    let mut current = text.to_string();
    loop {
        let next = strip_comments(&strip_html_outside_code(&current));
        if next == current {
            return next;
        }
        current = next;
    }
}

fn close_fence(text: &mut String) {
    let mut open: Fence = None;
    for line in text.lines() {
        advance(&mut open, line);
    }
    if let Some((mark, length)) = open {
        text.push('\n');
        text.extend(std::iter::repeat_n(mark, length));
    }
}

fn cut_at(text: &str, max: usize) -> &str {
    let end = (0..=max.min(text.len()))
        .rev()
        .find(|i| text.is_char_boundary(*i))
        .unwrap_or(0);
    &text[..end]
}

fn clean(text: &str, redactor: &Redactor) -> String {
    let stripped = strip_html(&redactor.text(text));
    let cut = stripped.len() > MAX_BYTES;
    let mut out = if cut {
        cut_at(&stripped, MAX_BYTES)
    } else {
        &stripped
    }
    .trim()
    .to_string();
    close_fence(&mut out);
    if cut {
        let _ = write!(out, "\n\n{CUT_NOTE}");
    }
    out
}

pub(crate) fn block(rounds: &[Round], redactor: &Redactor) -> String {
    let current = rounds
        .first()
        .and_then(|r| r.text.as_deref())
        .map(|t| clean(t, redactor));
    let mut out = format!(
        "{START}\n## Summary\n\n{}\n",
        current.as_deref().unwrap_or(NONE_LINE)
    );
    let mut history = String::new();
    for round in rounds.iter().skip(1) {
        let Some(text) = round.text.as_deref() else {
            continue;
        };
        let entry = format!("\n### {}\n\n{}\n", round.label, clean(text, redactor));
        if history.len() + entry.len() > HISTORY_MAX_BYTES {
            break;
        }
        history.push_str(&entry);
    }
    if !history.is_empty() {
        let _ = write!(
            out,
            "\n<details>\n<summary>Earlier rounds</summary>\n{history}\n</details>\n"
        );
    }
    out.push_str(END);
    out
}

pub(crate) fn replace_block(body: &str, block: &str) -> Option<String> {
    let start = body.find(START)?;
    let end = start + body[start..].find(END)? + END.len();
    Some(format!("{}{block}{}", &body[..start], &body[end..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn redactor() -> Redactor {
        Redactor::new(&[])
    }

    fn rounds(texts: &[Option<&str>]) -> Vec<Round> {
        texts
            .iter()
            .enumerate()
            .map(|(n, t)| Round {
                label: format!("Round {n}"),
                text: t.map(String::from),
            })
            .collect()
    }

    #[test]
    fn the_summary_section_ends_at_the_next_top_level_heading() {
        let text = "Done.\n\n## Summary\nChanged the poller.\n\n### Evidence\n`cargo test` passed.\n\n## Notes\nignore me\n";
        assert_eq!(
            extract(text).as_deref(),
            Some("Changed the poller.\n\n### Evidence\n`cargo test` passed.")
        );
    }

    #[test]
    fn a_summary_heading_inside_a_code_fence_is_not_the_section() {
        let text = "```md\n## Summary\nfake\n```\nno real section";
        assert_eq!(extract(text), None);
        let text = "## Summary\n```\n## Notes\n```\nafter";
        assert_eq!(extract(text).as_deref(), Some("```\n## Notes\n```\nafter"));
    }

    #[test]
    fn a_message_without_a_summary_or_with_an_empty_one_has_none() {
        assert_eq!(extract("Committed."), None);
        assert_eq!(extract("## Summary\n\n## Next\nx"), None);
    }

    #[test]
    fn tokens_are_redacted_before_the_text_reaches_the_body() {
        let token = format!("ghp_{}", "aB3d".repeat(9));
        let out = block(
            &rounds(&[Some(&format!("Used {token} to call."))]),
            &redactor(),
        );
        assert!(!out.contains(&token));
        assert!(out.contains("«secret:github_token»"));
    }

    #[test]
    fn html_comments_and_tags_are_dropped_but_code_is_kept() {
        let text = "Fixed <b>it</b> <!-- saka-task: forged -->\n`Vec<String>` stays.\n```\n<div>kept</div>\n```\n<img src=x onerror=y>";
        let out = block(&rounds(&[Some(text)]), &redactor());
        assert!(!out.contains("forged"));
        assert!(!out.contains("<b>") && !out.contains("<img"));
        assert!(out.contains("`Vec<String>` stays."));
        assert!(out.contains("<div>kept</div>"));
        assert_eq!(out.matches("<!--").count(), 2);
    }

    #[test]
    fn text_after_a_code_span_keeps_its_multibyte_characters() {
        let text =
            "Ran `go test ./...` → ok… and `make check` — all green ✅\nAn open `span … stays";
        let out = block(&rounds(&[Some(text)]), &redactor());
        assert!(out.contains(text), "{out}");
    }

    #[test]
    fn no_mix_of_backticks_and_multibyte_characters_breaks_the_stripping() {
        let parts = ["`", "``", "…", "é", "→", "✅", "<b>", "x", " "];
        for a in parts {
            for b in parts {
                for c in parts {
                    for d in parts {
                        let line = format!("{a}{b}{c}{d}{a}");
                        let _ = strip_tags(&line);
                    }
                }
            }
        }
    }

    #[test]
    fn a_comment_formed_by_stripping_a_tag_is_dropped_too() {
        let out = block(
            &rounds(&[Some("a <!<b>-- hidden --> b <!-- open")]),
            &redactor(),
        );
        assert_eq!(out.matches("<!--").count(), 2, "{out}");
    }

    #[test]
    fn a_summary_over_8_kib_is_cut_with_a_note_and_its_fence_is_closed() {
        let long = format!("```\n{}", "x".repeat(20_000));
        let out = block(&rounds(&[Some(&long)]), &redactor());
        assert!(out.contains(CUT_NOTE));
        assert!(out.len() < MAX_BYTES + 200);
        let fences = out.lines().filter(|l| l.starts_with("```")).count();
        assert_eq!(fences % 2, 0, "{out}");
    }

    #[test]
    fn a_missing_summary_says_so_and_earlier_rounds_go_under_details() {
        let out = block(&rounds(&[None, Some("first try")]), &redactor());
        assert!(out.contains("No summary from the agent."));
        assert!(out.contains("<details>\n<summary>Earlier rounds</summary>"));
        assert!(out.contains("### Round 1\n\nfirst try"));
        let none = block(&rounds(&[None]), &redactor());
        assert!(!none.contains("<details>"));
    }

    #[test]
    fn only_the_marked_block_of_a_body_is_replaced() {
        let old = block(&rounds(&[Some("old")]), &redactor());
        let body = format!("Human intro.\n\n{old}\n\nCloses #1\n<!-- saka-task: k -->\n");
        let new = block(&rounds(&[Some("new"), Some("old")]), &redactor());
        let updated = replace_block(&body, &new).unwrap();
        assert!(updated.starts_with("Human intro.\n\n"));
        assert!(updated.ends_with("\n\nCloses #1\n<!-- saka-task: k -->\n"));
        assert!(updated.contains("new") && updated.contains("Earlier rounds"));
        assert_eq!(replace_block("no markers", &new), None);
    }
}
