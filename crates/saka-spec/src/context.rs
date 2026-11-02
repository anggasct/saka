use std::collections::HashSet;

use saka_core::vocab::ContextOrigin;

use crate::frontmatter;
use crate::lint::{LintCode, LintIssue};
use crate::model::Link;

pub const MAX_LINKS: usize = 256;
pub const MAX_CONTEXT_DOCS: usize = 32;
pub const MAX_CONTEXT_DOC_BYTES: usize = 256 * 1024;
pub const MAX_CONTEXT_BYTES: usize = 256 * 1024;
pub const MAX_ALWAYS_CANDIDATES: usize = 64;

const DOC_EXTENSIONS: [&str; 2] = [".md", ".markdown"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolved {
    Doc(String),
    NotDoc(String),
    Outside,
    SelfLink,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wanted {
    pub path: String,
    pub origin: ContextOrigin,
    pub line: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fetch {
    Missing,
    TooLarge(u64),
    Bytes(Vec<u8>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fetched {
    pub wanted: Wanted,
    pub fetch: Fetch,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextDoc {
    pub path: String,
    pub origin: ContextOrigin,
    pub line: u32,
    pub content: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Included(usize),
    Failed(LintCode),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reference {
    pub path: String,
    pub origin: ContextOrigin,
    pub status: Status,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Context {
    pub docs: Vec<ContextDoc>,
    pub references: Vec<Reference>,
    pub issues: Vec<LintIssue>,
}

#[must_use]
pub fn resolve(spec_path: &str, target: &str) -> Resolved {
    if target.contains(['\0', '\\']) {
        return Resolved::Outside;
    }
    let (base, rest) = match target.strip_prefix('/') {
        Some(rest) => ("", rest),
        None => (
            spec_path.rsplit_once('/').map_or("", |(dir, _)| dir),
            target,
        ),
    };
    let mut parts: Vec<&str> = base.split('/').filter(|p| !p.is_empty()).collect();
    for part in rest.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if parts.pop().is_none() {
                    return Resolved::Outside;
                }
            }
            _ => parts.push(part),
        }
    }
    if parts.is_empty() {
        return Resolved::Outside;
    }
    let path = parts.join("/");
    if path == spec_path {
        return Resolved::SelfLink;
    }
    let lower = path.to_ascii_lowercase();
    if DOC_EXTENSIONS.iter().any(|ext| lower.ends_with(ext)) {
        Resolved::Doc(path)
    } else {
        Resolved::NotDoc(path)
    }
}

#[must_use]
pub fn is_always(bytes: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return false;
    };
    let Some(pairs) = frontmatter::top_level(text) else {
        return false;
    };
    let always = pairs.iter().any(|(k, v)| *k == "context" && v == "always");
    let spec = pairs.iter().any(|(k, v)| *k == "saka" && v == "spec");
    always && !spec
}

#[must_use]
pub fn linked(spec_path: &str, links: &[Link]) -> (Vec<Wanted>, Vec<LintIssue>) {
    let mut wanted = Vec::new();
    let mut issues = Vec::new();
    let mut seen = HashSet::new();
    for link in links {
        match resolve(spec_path, &link.target) {
            Resolved::Doc(path) => {
                if seen.insert(path.clone()) {
                    wanted.push(Wanted {
                        path,
                        origin: ContextOrigin::Link,
                        line: link.line,
                    });
                }
            }
            Resolved::NotDoc(path) => {
                if seen.insert(path.clone()) {
                    issues.push(
                        LintIssue::new(LintCode::ReferenceNotDoc, link.line).with("path", path),
                    );
                }
            }
            Resolved::Outside => issues.push(
                LintIssue::new(LintCode::ReferenceOutsideWorkspace, link.line)
                    .with("target", link.target.clone()),
            ),
            Resolved::SelfLink => {}
        }
    }
    (wanted, issues)
}

#[must_use]
pub fn always(linked: &[Wanted], mut candidates: Vec<String>) -> (Vec<Wanted>, Vec<LintIssue>) {
    candidates.sort();
    candidates.dedup();
    if candidates.len() > MAX_ALWAYS_CANDIDATES {
        let issue = LintIssue::new(LintCode::ContextTooLarge, 1)
            .with("reason", "always_candidates")
            .with("count", candidates.len())
            .with("limit", MAX_ALWAYS_CANDIDATES);
        return (Vec::new(), vec![issue]);
    }
    let wanted = candidates
        .into_iter()
        .filter(|path| !linked.iter().any(|w| &w.path == path))
        .map(|path| Wanted {
            path,
            origin: ContextOrigin::Always,
            line: 1,
        })
        .collect();
    (wanted, Vec::new())
}

#[must_use]
pub fn assemble(fetched: Vec<Fetched>, mut issues: Vec<LintIssue>) -> Context {
    let mut docs = Vec::new();
    let mut references = Vec::new();
    for Fetched { wanted, fetch } in fetched {
        let too_large = |bytes: u64| {
            LintIssue::new(LintCode::ReferenceTooLarge, wanted.line)
                .with("origin", wanted.origin.as_str())
                .with("bytes", bytes)
                .with("limit", MAX_CONTEXT_DOC_BYTES)
        };
        let (failure, content) = match fetch {
            Fetch::Missing => (
                Some(LintIssue::new(LintCode::ReferenceMissing, wanted.line)),
                None,
            ),
            Fetch::TooLarge(bytes) => (Some(too_large(bytes)), None),
            Fetch::Bytes(b) if b.len() > MAX_CONTEXT_DOC_BYTES => {
                (Some(too_large(b.len() as u64)), None)
            }
            Fetch::Bytes(b) if std::str::from_utf8(&b).is_err() => (
                Some(
                    LintIssue::new(LintCode::ReferenceNotText, wanted.line)
                        .with("origin", wanted.origin.as_str()),
                ),
                None,
            ),
            Fetch::Bytes(b) => (None, Some(b)),
        };
        match (failure, content) {
            (Some(issue), _) => {
                references.push(Reference {
                    path: wanted.path.clone(),
                    origin: wanted.origin,
                    status: Status::Failed(issue.code),
                });
                issues.push(issue.with("path", wanted.path));
            }
            (None, Some(content)) => {
                references.push(Reference {
                    path: wanted.path.clone(),
                    origin: wanted.origin,
                    status: Status::Included(docs.len()),
                });
                docs.push(ContextDoc {
                    path: wanted.path,
                    origin: wanted.origin,
                    line: wanted.line,
                    content,
                });
            }
            (None, None) => {}
        }
    }
    let total: usize = docs.iter().map(|d| d.content.len()).sum();
    let over = if docs.len() > MAX_CONTEXT_DOCS {
        Some(
            LintIssue::new(LintCode::ContextTooLarge, 1)
                .with("reason", "docs")
                .with("count", docs.len())
                .with("limit", MAX_CONTEXT_DOCS),
        )
    } else if total > MAX_CONTEXT_BYTES {
        Some(
            LintIssue::new(LintCode::ContextTooLarge, 1)
                .with("reason", "bytes")
                .with("bytes", total)
                .with("limit", MAX_CONTEXT_BYTES),
        )
    } else {
        None
    };
    issues.extend(over);
    if issues.iter().any(|i| i.code == LintCode::ContextTooLarge) {
        docs.clear();
        for reference in &mut references {
            if matches!(reference.status, Status::Included(_)) {
                reference.status = Status::Failed(LintCode::ContextTooLarge);
            }
        }
    }
    issues.sort_by_key(|i| i.line);
    Context {
        docs,
        references,
        issues,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(target: &str, line: u32) -> Link {
        Link {
            target: target.into(),
            line,
        }
    }

    fn wanted(path: &str, origin: ContextOrigin, line: u32) -> Wanted {
        Wanted {
            path: path.into(),
            origin,
            line,
        }
    }

    #[test]
    fn targets_resolve_from_the_spec_directory_or_the_repo_root() {
        let spec = "docs/specs/a.md";
        assert_eq!(
            resolve(spec, "../arch.md"),
            Resolved::Doc("docs/arch.md".into())
        );
        assert_eq!(
            resolve(spec, "./b.markdown"),
            Resolved::Doc("docs/specs/b.markdown".into())
        );
        assert_eq!(
            resolve(spec, "/README.MD"),
            Resolved::Doc("README.MD".into())
        );
        assert_eq!(
            resolve(spec, "../../src/main.rs"),
            Resolved::NotDoc("src/main.rs".into())
        );
        assert_eq!(resolve(spec, "../"), Resolved::NotDoc("docs".into()));
        assert_eq!(resolve(spec, "a.md"), Resolved::SelfLink);
        assert_eq!(resolve("a.md", "b.md"), Resolved::Doc("b.md".into()));
    }

    #[test]
    fn targets_that_leave_the_repo_are_outside() {
        let spec = "docs/specs/a.md";
        for target in [
            "../../../x.md",
            "/../x.md",
            "a\\b.md",
            "x\0.md",
            "/",
            "..//..",
        ] {
            assert_eq!(resolve(spec, target), Resolved::Outside, "{target:?}");
        }
    }

    #[test]
    fn linked_docs_are_unique_and_other_files_warn() {
        let links = [
            link("../arch.md", 3),
            link("../arch.md", 9),
            link("../../Cargo.toml", 4),
            link("../../Cargo.toml", 5),
            link("../../../x.md", 6),
            link("a.md", 7),
        ];
        let (wanted_docs, issues) = linked("docs/specs/a.md", &links);
        assert_eq!(
            wanted_docs,
            vec![wanted("docs/arch.md", ContextOrigin::Link, 3)]
        );
        let codes: Vec<(LintCode, u32)> = issues.iter().map(|i| (i.code, i.line)).collect();
        assert_eq!(
            codes,
            vec![
                (LintCode::ReferenceNotDoc, 4),
                (LintCode::ReferenceOutsideWorkspace, 6)
            ]
        );
        assert!(!issues[0].is_error());
        assert_eq!(issues[1].params["target"], "../../../x.md");
    }

    #[test]
    fn always_docs_need_a_context_key_and_are_not_specs() {
        assert!(is_always(b"---\ncontext: always\n---\n# A\n"));
        assert!(is_always(
            b"---\r\ntitle: x\r\ncontext: \"always\" # all\r\n...\r\n"
        ));
        for bytes in [
            &b"# A\ncontext: always\n"[..],
            b"---\ncontext: sometimes\n---\n",
            b"---\ncontext: always\nsaka: spec\n---\n",
            b"---\ncontext: always\n",
            b"---\n  context: always\n---\n",
            b"\xff",
        ] {
            assert!(!is_always(bytes), "{bytes:?}");
        }
    }

    #[test]
    fn always_docs_follow_links_in_path_order_without_repeats() {
        let linked_docs = [wanted("docs/b.md", ContextOrigin::Link, 2)];
        let (docs, issues) = always(
            &linked_docs,
            vec!["docs/c.md".into(), "docs/b.md".into(), "docs/a.md".into()],
        );
        assert!(issues.is_empty());
        assert_eq!(
            docs,
            vec![
                wanted("docs/a.md", ContextOrigin::Always, 1),
                wanted("docs/c.md", ContextOrigin::Always, 1)
            ]
        );
        let many = (0..=MAX_ALWAYS_CANDIDATES)
            .map(|i| format!("d/{i}.md"))
            .collect();
        let (docs, issues) = always(&[], many);
        assert!(docs.is_empty());
        assert_eq!(issues[0].code, LintCode::ContextTooLarge);
        assert_eq!(issues[0].params["reason"], "always_candidates");
    }

    #[test]
    fn each_document_is_checked_before_the_totals() {
        let fetched = vec![
            Fetched {
                wanted: wanted("a.md", ContextOrigin::Link, 3),
                fetch: Fetch::Bytes(b"# A\n".to_vec()),
            },
            Fetched {
                wanted: wanted("gone.md", ContextOrigin::Link, 4),
                fetch: Fetch::Missing,
            },
            Fetched {
                wanted: wanted("bin.md", ContextOrigin::Always, 1),
                fetch: Fetch::Bytes(vec![0xff]),
            },
            Fetched {
                wanted: wanted("big.md", ContextOrigin::Link, 5),
                fetch: Fetch::TooLarge(MAX_CONTEXT_DOC_BYTES as u64 + 1),
            },
        ];
        let context = assemble(fetched, Vec::new());
        assert_eq!(context.docs.len(), 1);
        assert_eq!(context.docs[0].path, "a.md");
        let codes: Vec<(LintCode, u32)> = context.issues.iter().map(|i| (i.code, i.line)).collect();
        assert_eq!(
            codes,
            vec![
                (LintCode::ReferenceNotText, 1),
                (LintCode::ReferenceMissing, 4),
                (LintCode::ReferenceTooLarge, 5)
            ]
        );
        assert_eq!(context.issues[0].params["origin"], "always");
        assert_eq!(context.issues[2].params["bytes"], MAX_CONTEXT_DOC_BYTES + 1);
        assert_eq!(
            context
                .references
                .iter()
                .map(|r| r.status.clone())
                .collect::<Vec<_>>(),
            vec![
                Status::Included(0),
                Status::Failed(LintCode::ReferenceMissing),
                Status::Failed(LintCode::ReferenceNotText),
                Status::Failed(LintCode::ReferenceTooLarge)
            ]
        );
    }

    #[test]
    fn too_many_or_too_large_documents_keep_none() {
        let docs = |n: usize, size: usize| -> Vec<Fetched> {
            (0..n)
                .map(|i| Fetched {
                    wanted: wanted(&format!("{i}.md"), ContextOrigin::Link, 2),
                    fetch: Fetch::Bytes(vec![b'x'; size]),
                })
                .collect()
        };
        let context = assemble(docs(MAX_CONTEXT_DOCS + 1, 1), Vec::new());
        assert!(context.docs.is_empty());
        assert_eq!(context.issues.len(), 1);
        assert_eq!(context.issues[0].params["reason"], "docs");
        assert!(
            context
                .references
                .iter()
                .all(|r| r.status == Status::Failed(LintCode::ContextTooLarge))
        );

        let context = assemble(docs(2, MAX_CONTEXT_BYTES / 2 + 1), Vec::new());
        assert!(context.docs.is_empty());
        assert_eq!(context.issues[0].params["reason"], "bytes");

        let fits = assemble(
            docs(MAX_CONTEXT_DOCS, MAX_CONTEXT_BYTES / MAX_CONTEXT_DOCS),
            Vec::new(),
        );
        assert_eq!(fits.docs.len(), MAX_CONTEXT_DOCS);
        assert!(fits.issues.is_empty());
    }

    #[test]
    fn an_always_candidate_overflow_keeps_no_documents() {
        let issue =
            LintIssue::new(LintCode::ContextTooLarge, 1).with("reason", "always_candidates");
        let fetched = vec![Fetched {
            wanted: wanted("a.md", ContextOrigin::Link, 3),
            fetch: Fetch::Bytes(b"# A\n".to_vec()),
        }];
        let context = assemble(fetched, vec![issue]);
        assert!(context.docs.is_empty());
        assert_eq!(
            context.references[0].status,
            Status::Failed(LintCode::ContextTooLarge)
        );
    }
}
