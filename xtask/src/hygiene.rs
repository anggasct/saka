use std::fmt;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};
use ra_ap_rustc_lexer::{FrontmatterAllowed, TokenKind, tokenize};
use regex::Regex;

const MAX_COMMENT_LINES: usize = 3;

const SKIPPED_PREFIXES: &[&str] = &["spikes/", "xtask/fixtures/", "web/eslint-fixtures/"];
const SKIPPED_FILES: &[&str] = &[
    ".gitignore",
    "Cargo.lock",
    "web/pnpm-lock.yaml",
    "web/src/routeTree.gen.ts",
    "web/src/api/schema.d.ts",
    "crates/saka-api/openapi.json",
];
const TERM_SOURCES: &[&str] = &["xtask/src/hygiene.rs"];
const API_DOC_SOURCES: &[&str] = &["crates/saka-api/src/dto.rs"];
const TEXT_SUFFIXES: &[&str] = &[
    "rs", "ts", "tsx", "js", "mjs", "md", "toml", "yml", "yaml", "json", "html", "css", "sh",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rule {
    DocComment,
    BlockComment,
    LongComment,
    TaskMarker,
    CommentedOutCode,
    InternalTerm,
}

impl fmt::Display for Rule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::DocComment => "doc comment",
            Self::BlockComment => "block comment",
            Self::LongComment => "comment longer than 3 lines",
            Self::TaskMarker => "task marker",
            Self::CommentedOutCode => "commented-out code",
            Self::InternalTerm => "internal delivery reference",
        })
    }
}

struct Patterns {
    task_marker: Regex,
    internal_term: Regex,
}

impl Patterns {
    fn new() -> Result<Self> {
        Ok(Self {
            task_marker: Regex::new(r"\b(TODO|FIXME|XXX|HACK)\b")?,
            internal_term: Regex::new(
                r"(?i)project-docs|development-plan|delivery[ -]queue|delivery[ -]os|claim protocol|\bhermes\b|\bkanban\b|\bIMP-\d{3}\b|\bD-\d{3}\b|\bADR-\d{3}\b",
            )?,
        })
    }
}

struct Comment<'a> {
    line: usize,
    text: &'a str,
}

fn line_comment_findings(patterns: &Patterns, comments: &[Comment<'_>]) -> Vec<(usize, Rule)> {
    let mut findings = Vec::new();
    let mut run: Option<(usize, usize, usize)> = None;
    for comment in comments {
        if patterns.task_marker.is_match(comment.text) {
            findings.push((comment.line, Rule::TaskMarker));
        }
        if comment.text.trim_end().ends_with([';', '{', '}']) {
            findings.push((comment.line, Rule::CommentedOutCode));
        }
        run = match run {
            Some((start, last, count)) if comment.line == last + 1 => {
                Some((start, comment.line, count + 1))
            }
            _ => Some((comment.line, comment.line, 1)),
        };
        if let Some((start, _, count)) = run
            && count == MAX_COMMENT_LINES + 1
        {
            findings.push((start, Rule::LongComment));
        }
    }
    findings
}

fn is_doc_attribute(significant: &[(TokenKind, &str)]) -> bool {
    let kinds: Vec<TokenKind> = significant.iter().map(|(kind, _)| *kind).collect();
    let rest = match kinds.as_slice() {
        [
            TokenKind::Pound,
            TokenKind::Bang,
            TokenKind::OpenBracket,
            ..,
        ] => &significant[3..],
        [TokenKind::Pound, TokenKind::OpenBracket, ..] => &significant[2..],
        _ => return false,
    };
    matches!(rest, [(TokenKind::Ident, "doc"), (TokenKind::Eq, _), ..])
}

fn rust_findings(patterns: &Patterns, source: &str) -> Vec<(usize, Rule)> {
    let mut findings = Vec::new();
    let mut comments = Vec::new();
    let mut significant: Vec<(TokenKind, &str, usize)> = Vec::new();
    let mut offset = 0;
    let mut line = 1;
    for token in tokenize(source, FrontmatterAllowed::No) {
        let len = token.len as usize;
        let text = source.get(offset..offset + len).unwrap_or_default();
        match token.kind {
            TokenKind::LineComment { doc_style: Some(_) }
            | TokenKind::BlockComment {
                doc_style: Some(_), ..
            } => findings.push((line, Rule::DocComment)),
            TokenKind::BlockComment { .. } => findings.push((line, Rule::BlockComment)),
            TokenKind::LineComment { doc_style: None } => comments.push(Comment {
                line,
                text: text.trim_start_matches('/'),
            }),
            TokenKind::Whitespace => {}
            kind => significant.push((kind, text, line)),
        }
        offset += len;
        line += text.matches('\n').count();
    }
    for (index, (_, _, at)) in significant.iter().enumerate() {
        let window: Vec<(TokenKind, &str)> = significant[index..]
            .iter()
            .take(5)
            .map(|(kind, text, _)| (*kind, *text))
            .collect();
        if is_doc_attribute(&window) {
            findings.push((*at, Rule::DocComment));
        }
    }
    findings.extend(line_comment_findings(patterns, &comments));
    findings
}

fn shell_findings(patterns: &Patterns, source: &str) -> Vec<(usize, Rule)> {
    let comments: Vec<Comment<'_>> = source
        .lines()
        .enumerate()
        .filter(|(index, text)| !(*index == 0 && text.starts_with("#!")))
        .filter_map(|(index, text)| {
            text.trim_start().strip_prefix('#').map(|rest| Comment {
                line: index + 1,
                text: rest,
            })
        })
        .collect();
    line_comment_findings(patterns, &comments)
}

fn term_findings(patterns: &Patterns, source: &str) -> Vec<(usize, Rule)> {
    source
        .lines()
        .enumerate()
        .filter(|(_, text)| patterns.internal_term.is_match(text))
        .map(|(index, _)| (index + 1, Rule::InternalTerm))
        .collect()
}

fn is_shell(path: &str, source: &str) -> bool {
    Path::new(path).extension().is_some_and(|e| e == "sh")
        || source
            .lines()
            .next()
            .is_some_and(|first| first.starts_with("#!") && first.ends_with("sh"))
}

fn check_file(patterns: &Patterns, path: &str, source: &str) -> Vec<(usize, Rule)> {
    let extension = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default();
    let mut findings = Vec::new();
    if extension == "rs" {
        let api_docs = API_DOC_SOURCES.contains(&path);
        findings.extend(
            rust_findings(patterns, source)
                .into_iter()
                .filter(|(_, rule)| !(api_docs && *rule == Rule::DocComment)),
        );
    } else if is_shell(path, source) {
        findings.extend(shell_findings(patterns, source));
    }
    let is_text = TEXT_SUFFIXES.contains(&extension) || extension.is_empty();
    if is_text && !TERM_SOURCES.contains(&path) {
        findings.extend(term_findings(patterns, source));
    }
    findings.sort_by_key(|(line, _)| *line);
    findings
}

fn tracked_files(root: &Path) -> Result<Vec<String>> {
    let out = Command::new("git")
        .args(["ls-files", "--cached", "--others", "--exclude-standard"])
        .current_dir(root)
        .output()
        .context("run git ls-files")?;
    if !out.status.success() {
        bail!(
            "git ls-files failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|path| {
            !SKIPPED_PREFIXES
                .iter()
                .any(|prefix| path.starts_with(prefix))
                && !SKIPPED_FILES.contains(path)
                && root.join(path).is_file()
        })
        .map(String::from)
        .collect())
}

pub fn run(root: &Path) -> Result<()> {
    let patterns = Patterns::new()?;
    let mut total = 0;
    for path in tracked_files(root)? {
        let Ok(source) = std::fs::read_to_string(root.join(&path)) else {
            continue;
        };
        for (line, rule) in check_file(&patterns, &path, &source) {
            eprintln!("  {path}:{line}: {rule}");
            total += 1;
        }
    }
    if total > 0 {
        bail!("{total} finding(s)");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(path: &str, source: &str) -> Vec<(usize, Rule)> {
        let patterns = Patterns::new().unwrap_or_else(|e| panic!("{e}"));
        check_file(&patterns, path, source)
    }

    #[test]
    fn doc_comments_and_doc_attributes_are_refused() {
        let source = "//! crate\n/// item\nfn a() {}\n/** block */\nfn b() {}\n#[doc = \"x\"]\nfn c() {}\n#[doc(hidden)]\nfn d() {}\n";
        assert_eq!(
            rules("a.rs", source),
            [
                (1, Rule::DocComment),
                (2, Rule::DocComment),
                (4, Rule::DocComment),
                (6, Rule::DocComment),
            ]
        );
    }

    #[test]
    fn a_short_why_comment_passes_and_a_long_one_does_not() {
        let four =
            "fn a() {\n    // one\n    // two\n    // three\n    let x = 1; // trailing\n}\n";
        assert_eq!(rules("a.rs", four), [(2, Rule::LongComment)]);
        let three = "fn a() {\n    // one\n    // two\n    // three\n    let x = 1;\n}\n";
        assert!(rules("a.rs", three).is_empty());
        let block = "fn a() { /* no */ }\n";
        assert_eq!(rules("a.rs", block), [(1, Rule::BlockComment)]);
    }

    #[test]
    fn comment_markers_inside_literals_are_code() {
        let source = "fn a<'a>(x: &'a str) -> [&'a str; 3] {\n    let c = '/';\n    [\"// TODO\", r#\"/// x\"#, \"/* y */\"]\n}\n";
        assert!(rules("a.rs", source).is_empty());
    }

    #[test]
    fn task_markers_and_commented_out_code_are_refused() {
        let source = "fn a() {\n    // TODO later\n    // let x = 1;\n}\n";
        assert_eq!(
            rules("a.rs", source),
            [(2, Rule::TaskMarker), (3, Rule::CommentedOutCode)]
        );
    }

    #[test]
    fn shell_comments_follow_the_same_rules_after_the_shebang() {
        let source = "#!/usr/bin/env bash\n# one\n# two\n# three\n# four\necho ok\n";
        assert_eq!(rules("scripts/run", source), [(2, Rule::LongComment)]);
    }

    #[test]
    fn api_descriptions_are_doc_comments_only_in_the_dto_module() {
        let source = "pub struct Page {\n    /// Redacted.\n    pub input: String,\n}\n";
        assert!(rules("crates/saka-api/src/dto.rs", source).is_empty());
        assert_eq!(
            rules("crates/saka-api/src/routes.rs", source),
            [(2, Rule::DocComment)]
        );
    }

    #[test]
    fn delivery_process_terms_are_refused_in_any_text_file() {
        assert_eq!(
            rules("README.md", "see project-docs/specs\n"),
            [(1, Rule::InternalTerm)]
        );
        assert_eq!(rules("a.ts", "// Delivery OS\n"), [(1, Rule::InternalTerm)]);
        assert!(rules("README.md", "acceptance criteria AC-01 parse\n").is_empty());
    }
}
