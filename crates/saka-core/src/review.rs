use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use crate::vocab::{ReviewBasis, ReviewOutcome, Severity};

pub const MAX_REVIEW_BODY_CHARS: usize = 60_000;
pub const MAX_VERDICT_BYTES: usize = 256 * 1024;
pub const MAX_SUMMARY_CHARS: usize = 4_000;
pub const MAX_FINDINGS: usize = 50;
pub const MAX_FINDING_CHARS: usize = 2_000;
pub const MAX_QUESTIONS: usize = 10;
pub const MAX_QUESTION_CHARS: usize = 500;
pub const MAX_PATH_CHARS: usize = 300;
pub const FORMAT_RETRIES: u32 = 1;

const FENCE: &str = "```";
const TRUNCATED_NOTE: &str = "\n\n_The review was cut at the size limit._\n";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    pub path: Option<String>,
    pub line: Option<u32>,
    pub severity: Severity,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Verdict {
    pub outcome: ReviewOutcome,
    pub summary: String,
    pub findings: Vec<Finding>,
    pub questions: Vec<String>,
}

impl Verdict {
    pub fn blocking_findings(&self) -> usize {
        self.findings
            .iter()
            .filter(|f| matches!(f.severity, Severity::Blocker | Severity::Major))
            .count()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum VerdictError {
    #[error("the reply is empty")]
    Empty,
    #[error("the reply is not JSON")]
    NotJson,
    #[error("the reply has text outside the verdict")]
    TextOutsideVerdict,
    #[error("the verdict does not have the required shape")]
    Shape,
    #[error("the verdict value is not one of approve, request_changes, spec_unclear")]
    UnknownVerdict,
    #[error("the verdict contradicts its findings or questions")]
    Inconsistent,
    #[error("the verdict is larger than the limits allow")]
    TooLarge,
    #[error("a finding path is not a relative path in the checkout")]
    PathInvalid,
    #[error("spec_unclear needs a spec to be unclear about")]
    SpecUnclearWithoutSpec,
}

impl VerdictError {
    pub fn code(self) -> &'static str {
        match self {
            VerdictError::Empty => "empty",
            VerdictError::NotJson => "not_json",
            VerdictError::TextOutsideVerdict => "text_outside_verdict",
            VerdictError::Shape => "shape",
            VerdictError::UnknownVerdict => "unknown_verdict",
            VerdictError::Inconsistent => "inconsistent",
            VerdictError::TooLarge => "too_large",
            VerdictError::PathInvalid => "path_invalid",
            VerdictError::SpecUnclearWithoutSpec => "spec_unclear_without_spec",
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Wire {
    verdict: String,
    summary: String,
    #[serde(default)]
    findings: Vec<WireFinding>,
    #[serde(default)]
    questions: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireFinding {
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    line: Option<u32>,
    severity: String,
    body: String,
}

fn unfenced(text: &str) -> Result<&str, VerdictError> {
    let Some(rest) = text.strip_prefix(FENCE) else {
        return Ok(text);
    };
    let rest = rest.strip_prefix("json").ok_or(VerdictError::Shape)?;
    let rest = rest
        .strip_prefix('\n')
        .or_else(|| rest.strip_prefix("\r\n"))
        .ok_or(VerdictError::Shape)?;
    let end = rest.rfind(FENCE).ok_or(VerdictError::TextOutsideVerdict)?;
    if !rest[end + FENCE.len()..].trim().is_empty() || rest[..end].contains(FENCE) {
        return Err(VerdictError::TextOutsideVerdict);
    }
    Ok(rest[..end].trim())
}

fn object(text: &str) -> Result<Wire, VerdictError> {
    let mut stream = serde_json::Deserializer::from_str(text).into_iter::<serde_json::Value>();
    let value = match stream.next() {
        Some(Ok(value)) => value,
        Some(Err(_)) | None => {
            return Err(if text.contains('{') {
                VerdictError::TextOutsideVerdict
            } else {
                VerdictError::NotJson
            });
        }
    };
    if !text[stream.byte_offset()..].trim().is_empty() {
        return Err(VerdictError::TextOutsideVerdict);
    }
    if !value.is_object() {
        return Err(VerdictError::Shape);
    }
    serde_json::from_value(value).map_err(|_| VerdictError::Shape)
}

fn path_ok(path: &str) -> bool {
    !path.is_empty()
        && path.chars().count() <= MAX_PATH_CHARS
        && !path.starts_with('/')
        && !path.contains('\\')
        && !path.chars().any(char::is_control)
        && !path.split('/').any(|part| part == "..")
}

fn finding(raw: WireFinding) -> Result<Finding, VerdictError> {
    let severity: Severity = raw.severity.parse().map_err(|_| VerdictError::Shape)?;
    if raw.body.trim().is_empty() {
        return Err(VerdictError::Shape);
    }
    if raw.body.chars().count() > MAX_FINDING_CHARS {
        return Err(VerdictError::TooLarge);
    }
    let path_valid = raw.path.as_deref().is_none_or(path_ok);
    let line_valid = raw.line.is_none_or(|l| l >= 1) && (raw.line.is_none() || raw.path.is_some());
    if !path_valid || !line_valid {
        return Err(VerdictError::PathInvalid);
    }
    Ok(Finding {
        path: raw.path,
        line: raw.line,
        severity,
        body: raw.body,
    })
}

pub fn parse(text: &str, has_spec: bool) -> Result<Verdict, VerdictError> {
    if text.len() > MAX_VERDICT_BYTES {
        return Err(VerdictError::TooLarge);
    }
    let text = text.trim();
    if text.is_empty() {
        return Err(VerdictError::Empty);
    }
    let wire = object(unfenced(text)?)?;
    let outcome = match wire.verdict.as_str() {
        "approve" => ReviewOutcome::Approved,
        "request_changes" => ReviewOutcome::ChangesRequested,
        "spec_unclear" => ReviewOutcome::SpecUnclear,
        _ => return Err(VerdictError::UnknownVerdict),
    };
    if outcome == ReviewOutcome::SpecUnclear && !has_spec {
        return Err(VerdictError::SpecUnclearWithoutSpec);
    }
    if wire.summary.trim().is_empty() {
        return Err(VerdictError::Shape);
    }
    if wire.summary.chars().count() > MAX_SUMMARY_CHARS
        || wire.findings.len() > MAX_FINDINGS
        || wire.questions.len() > MAX_QUESTIONS
    {
        return Err(VerdictError::TooLarge);
    }
    for question in &wire.questions {
        if question.trim().is_empty() {
            return Err(VerdictError::Shape);
        }
        if question.chars().count() > MAX_QUESTION_CHARS {
            return Err(VerdictError::TooLarge);
        }
    }
    let findings = wire
        .findings
        .into_iter()
        .map(finding)
        .collect::<Result<Vec<_>, _>>()?;
    let verdict = Verdict {
        outcome,
        summary: wire.summary,
        findings,
        questions: wire.questions,
    };
    let consistent = match outcome {
        ReviewOutcome::Approved => verdict.blocking_findings() == 0 && verdict.questions.is_empty(),
        ReviewOutcome::ChangesRequested => {
            verdict.blocking_findings() > 0 && verdict.questions.is_empty()
        }
        ReviewOutcome::SpecUnclear => !verdict.questions.is_empty() && verdict.findings.is_empty(),
    };
    if consistent {
        Ok(verdict)
    } else {
        Err(VerdictError::Inconsistent)
    }
}

fn neutralised(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        out.push(c);
        if c == '@'
            && chars
                .peek()
                .is_some_and(|n| n.is_alphanumeric() || *n == '_')
        {
            out.push('\u{200b}');
        }
    }
    out
}

fn headline(outcome: ReviewOutcome) -> &'static str {
    match outcome {
        ReviewOutcome::Approved => "approved",
        ReviewOutcome::ChangesRequested => "changes requested",
        ReviewOutcome::SpecUnclear => "spec unclear",
    }
}

fn cut(text: &mut String, max_chars: usize) {
    if let Some((index, _)) = text.char_indices().nth(max_chars) {
        text.truncate(index);
    }
}

#[must_use]
pub fn render_body(head_sha: &str, verdict: &Verdict, basis: ReviewBasis) -> String {
    let short: String = head_sha.chars().take(7).collect();
    let mut out = format!(
        "**Saka review @{short}: {}**\n\n{}\n",
        headline(verdict.outcome),
        neutralised(&verdict.summary)
    );
    if !verdict.findings.is_empty() {
        out.push_str("\n**Findings**\n\n");
        for f in &verdict.findings {
            let place = match (&f.path, f.line) {
                (Some(path), Some(line)) => format!("`{}:{line}` - ", neutralised(path)),
                (Some(path), None) => format!("`{}` - ", neutralised(path)),
                _ => String::new(),
            };
            let _ = writeln!(out, "- {place}{} - {}", f.severity, neutralised(&f.body));
        }
    }
    if !verdict.questions.is_empty() {
        out.push_str("\n**Questions about the spec**\n\n");
        for q in &verdict.questions {
            let _ = writeln!(out, "- {}", neutralised(q));
        }
    }
    if basis == ReviewBasis::None {
        out.push_str("\nNo spec: reviewed against the repository conventions.\n");
    }
    let _ = write!(
        out,
        "\n_Reviewed by an independent Saka reviewer session. The verdict applies to commit {head_sha}._\n"
    );
    if out.chars().count() > MAX_REVIEW_BODY_CHARS {
        cut(
            &mut out,
            MAX_REVIEW_BODY_CHARS - TRUNCATED_NOTE.chars().count(),
        );
        out.push_str(TRUNCATED_NOTE);
    }
    out
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use serde_json::json;

    use super::*;

    fn approve() -> String {
        json!({"verdict": "approve", "summary": "Looks right.", "findings": [
            {"path": "src/a.rs", "line": 3, "severity": "nit", "body": "rename"}], "questions": []})
        .to_string()
    }

    fn changes() -> String {
        json!({"verdict": "request_changes", "summary": "No backoff.", "findings": [
            {"path": "src/poller.rs", "line": 42, "severity": "major", "body": "loops hot"}]})
        .to_string()
    }

    fn unclear() -> String {
        json!({"verdict": "spec_unclear", "summary": "AC-2 is ambiguous.", "questions": ["Which unit?"]})
            .to_string()
    }

    fn code(text: &str, has_spec: bool) -> &'static str {
        parse(text, has_spec).unwrap_err().code()
    }

    #[test]
    fn the_three_outcomes_parse_bare_and_in_one_json_fence() {
        for (text, outcome) in [
            (approve(), ReviewOutcome::Approved),
            (changes(), ReviewOutcome::ChangesRequested),
            (unclear(), ReviewOutcome::SpecUnclear),
        ] {
            assert_eq!(parse(&text, true).unwrap().outcome, outcome);
            let fenced = format!("```json\n{text}\n```\n");
            assert_eq!(parse(&fenced, true).unwrap().outcome, outcome);
            assert_eq!(
                parse(&format!("  \n{text}\n "), true).unwrap().outcome,
                outcome
            );
        }
    }

    #[test]
    fn text_around_the_verdict_is_refused() {
        let ok = approve();
        for text in [
            format!("Here you go: {ok}"),
            format!("{ok}\nThanks!"),
            format!("{ok}{ok}"),
            format!("```json\n{ok}\n```\nbye"),
            format!("hi\n```json\n{ok}\n```"),
            format!("```json\n{ok}\n```\n```json\n{ok}\n```"),
        ] {
            assert_eq!(code(&text, true), "text_outside_verdict", "{text}");
        }
    }

    #[test]
    fn malformed_replies_get_their_own_codes() {
        assert_eq!(code("  \n", true), "empty");
        assert_eq!(code("I approve.", true), "not_json");
        assert_eq!(code("[1]", true), "shape");
        assert_eq!(code("\"approve\"", true), "shape");
        assert_eq!(code("```python\nx\n```", true), "shape");
        let unknown = json!({"verdict": "lgtm", "summary": "s"}).to_string();
        assert_eq!(code(&unknown, true), "unknown_verdict");
        let extra = json!({"verdict": "approve", "summary": "s", "head_sha": "x"}).to_string();
        assert_eq!(code(&extra, true), "shape");
        let missing = json!({"verdict": "approve"}).to_string();
        assert_eq!(code(&missing, true), "shape");
        let severity = json!({"verdict": "approve", "summary": "s", "findings": [
            {"severity": "huge", "body": "x"}]})
        .to_string();
        assert_eq!(code(&severity, true), "shape");
    }

    #[test]
    fn a_verdict_must_agree_with_its_findings_and_questions() {
        let approve_blocker = json!({"verdict": "approve", "summary": "s", "findings": [
            {"severity": "blocker", "body": "x"}]})
        .to_string();
        let approve_question =
            json!({"verdict": "approve", "summary": "s", "questions": ["q"]}).to_string();
        let changes_nit = json!({"verdict": "request_changes", "summary": "s", "findings": [
            {"severity": "nit", "body": "x"}]})
        .to_string();
        let changes_none = json!({"verdict": "request_changes", "summary": "s"}).to_string();
        let unclear_none = json!({"verdict": "spec_unclear", "summary": "s"}).to_string();
        let unclear_findings = json!({"verdict": "spec_unclear", "summary": "s",
            "questions": ["q"], "findings": [{"severity": "nit", "body": "x"}]})
        .to_string();
        for text in [
            approve_blocker,
            approve_question,
            changes_nit,
            changes_none,
            unclear_none,
            unclear_findings,
        ] {
            assert_eq!(code(&text, true), "inconsistent", "{text}");
        }
    }

    #[test]
    fn spec_unclear_is_refused_when_there_is_no_spec() {
        assert_eq!(code(&unclear(), false), "spec_unclear_without_spec");
        assert!(parse(&approve(), false).is_ok());
    }

    #[test]
    fn limits_and_paths_are_enforced() {
        let wrap = |finding: serde_json::Value| {
            json!({"verdict": "approve", "summary": "s", "findings": [finding]}).to_string()
        };
        for path in ["/etc/passwd", "../x", "a/../b", "a\\b", ""] {
            let text = wrap(json!({"path": path, "severity": "nit", "body": "x"}));
            assert_eq!(code(&text, true), "path_invalid", "{path:?}");
        }
        assert_eq!(
            code(
                &wrap(json!({"line": 3, "severity": "nit", "body": "x"})),
                true
            ),
            "path_invalid"
        );
        assert_eq!(
            code(
                &wrap(json!({"path": "a", "line": 0, "severity": "nit", "body": "x"})),
                true
            ),
            "path_invalid"
        );
        let long = "x".repeat(MAX_SUMMARY_CHARS + 1);
        let text = json!({"verdict": "approve", "summary": long}).to_string();
        assert_eq!(code(&text, true), "too_large");
        let many: Vec<_> = (0..=MAX_FINDINGS)
            .map(|_| json!({"severity": "nit", "body": "x"}))
            .collect();
        let text = json!({"verdict": "approve", "summary": "s", "findings": many}).to_string();
        assert_eq!(code(&text, true), "too_large");
        assert_eq!(code(&" ".repeat(MAX_VERDICT_BYTES + 1), true), "too_large");
    }

    #[test]
    fn the_body_names_the_head_the_findings_and_a_missing_spec() {
        let verdict = parse(&changes(), true).unwrap();
        let body = render_body(&"ab".repeat(20), &verdict, ReviewBasis::None);
        assert!(body.starts_with("**Saka review @abababa: changes requested**"));
        assert!(body.contains("- `src/poller.rs:42` - major - loops hot"));
        assert!(body.contains("No spec: reviewed against the repository conventions."));
        let with_spec = render_body(&"ab".repeat(20), &verdict, ReviewBasis::Step);
        assert!(!with_spec.contains("No spec"));
    }

    #[test]
    fn mentions_in_model_text_are_neutralised() {
        let text = json!({"verdict": "approve", "summary": "ping @alice and a@b.c, @ alone",
            "findings": [{"path": "@x/y", "severity": "nit", "body": "cc @team/core"}]})
        .to_string();
        let body = render_body(
            &"a".repeat(40),
            &parse(&text, true).unwrap(),
            ReviewBasis::Spec,
        );
        assert!(body.contains("@\u{200b}alice"));
        assert!(body.contains("@\u{200b}team/core"));
        assert!(body.contains("`@\u{200b}x/y`"));
        assert!(body.contains("@ alone"));
        assert!(!body.contains("@alice"));
    }

    #[test]
    fn an_oversized_body_is_cut_with_a_note() {
        let findings: Vec<_> = (0..MAX_FINDINGS)
            .map(|i| json!({"path": format!("f{i}.rs"), "severity": "nit", "body": "é".repeat(MAX_FINDING_CHARS)}))
            .collect();
        let text = json!({"verdict": "approve", "summary": "s", "findings": findings}).to_string();
        let body = render_body(
            &"a".repeat(40),
            &parse(&text, true).unwrap(),
            ReviewBasis::Spec,
        );
        assert!(body.chars().count() <= MAX_REVIEW_BODY_CHARS);
        assert!(body.ends_with(TRUNCATED_NOTE));
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(512))]

        #[test]
        fn any_text_parses_or_fails_without_panicking(text in "\\PC{0,200}") {
            let _ = parse(&text, true);
            let _ = parse(&format!("```json\n{text}\n```"), false);
        }

        #[test]
        fn a_valid_verdict_with_any_stray_text_around_it_is_refused(
            before in "[a-zA-Z0-9,.!]{1,20}",
            after in "[a-zA-Z0-9,.!]{1,20}",
        ) {
            let ok = approve();
            let leading = format!("{before}{ok}");
            let trailing = format!("{ok}{after}");
            let fenced = format!("```json\n{ok}\n```\n{after}");
            prop_assert!(parse(&leading, true).is_err());
            prop_assert!(parse(&trailing, true).is_err());
            prop_assert!(parse(&fenced, true).is_err());
        }
    }
}
