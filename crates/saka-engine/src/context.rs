use std::collections::HashMap;

use saka_core::model::ContextRef;
use saka_ports::{Blob, RepoRef};
use saka_spec::context::{
    self, Context, Fetch, Fetched, MAX_ALWAYS_CANDIDATES, MAX_CONTEXT_DOC_BYTES, Status,
};
use saka_spec::{LintIssue, Parsed};
use sha2::{Digest, Sha256};

use crate::{Engine, Result};

#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct PinnedContext {
    pub refs: Vec<ContextRef>,
    pub blobs: Vec<(String, Vec<u8>)>,
    pub hash: String,
    pub issues: Vec<LintIssue>,
}

pub(crate) fn sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    format!("sha256:{hex}")
}

pub(crate) fn pin(context: Context) -> PinnedContext {
    let hashes: Vec<String> = context.docs.iter().map(|d| sha256(&d.content)).collect();
    let mut identity = String::new();
    for reference in &context.references {
        let status = match &reference.status {
            Status::Included(i) => hashes.get(*i).map_or("", String::as_str),
            Status::Failed(code) => code.as_str(),
        };
        identity.push_str(&format!(
            "{}\t{}\t{status}\n",
            reference.origin.as_str(),
            reference.path
        ));
    }
    let refs = context
        .docs
        .iter()
        .zip(&hashes)
        .map(|(doc, hash)| ContextRef {
            path: doc.path.clone(),
            origin: doc.origin,
            line: doc.line,
            bytes: doc.content.len() as u64,
            content_hash: hash.clone(),
        })
        .collect();
    let blobs = context
        .docs
        .into_iter()
        .zip(hashes)
        .map(|(doc, hash)| (hash, doc.content))
        .collect();
    PinnedContext {
        refs,
        blobs,
        hash: sha256(identity.as_bytes()),
        issues: context.issues,
    }
}

fn fetch_of(blob: Blob) -> Fetch {
    match blob {
        Blob::Missing => Fetch::Missing,
        Blob::TooLarge { bytes } => Fetch::TooLarge(bytes),
        Blob::Bytes(bytes) => Fetch::Bytes(bytes),
    }
}

impl Engine {
    pub(crate) async fn resolve_context(
        &self,
        repo: &RepoRef,
        project_path: &str,
        spec_path: &str,
        commit: &str,
        parsed: &Parsed,
    ) -> Result<PinnedContext> {
        let Some(model) = &parsed.model else {
            return Ok(pin(Context::default()));
        };
        let limit = MAX_CONTEXT_DOC_BYTES as u64;
        let (linked, mut issues) = context::linked(spec_path, &model.links);
        let candidates = self
            .ports
            .docs
            .find_always(repo, commit, project_path)
            .await?;
        let mut read = HashMap::new();
        let passing = if candidates.len() > MAX_ALWAYS_CANDIDATES {
            candidates
        } else {
            let mut passing = Vec::new();
            for path in candidates.into_iter().filter(|p| p != spec_path) {
                match self.ports.docs.read_at(repo, commit, &path, limit).await? {
                    Blob::Bytes(bytes) if context::is_always(&bytes) => {
                        read.insert(path.clone(), Fetch::Bytes(bytes));
                        passing.push(path);
                    }
                    Blob::TooLarge { bytes } => {
                        read.insert(path.clone(), Fetch::TooLarge(bytes));
                        passing.push(path);
                    }
                    Blob::Bytes(_) | Blob::Missing => {}
                }
            }
            passing
        };
        let (always, always_issues) = context::always(&linked, passing);
        issues.extend(always_issues);
        let mut fetched = Vec::with_capacity(linked.len() + always.len());
        for wanted in linked {
            let blob = self
                .ports
                .docs
                .read_at(repo, commit, &wanted.path, limit)
                .await?;
            fetched.push(Fetched {
                wanted,
                fetch: fetch_of(blob),
            });
        }
        for wanted in always {
            let fetch = read.remove(&wanted.path).unwrap_or(Fetch::Missing);
            fetched.push(Fetched { wanted, fetch });
        }
        Ok(pin(context::assemble(fetched, issues)))
    }
}

#[cfg(test)]
mod tests {
    use saka_core::vocab::ContextOrigin;
    use saka_spec::LintCode;
    use saka_spec::context::{ContextDoc, Reference};

    use super::*;

    #[test]
    fn an_empty_context_hashes_the_empty_string() {
        assert_eq!(
            pin(Context::default()).hash,
            saka_core::event::EMPTY_CONTEXT_HASH
        );
    }

    #[test]
    fn the_context_hash_covers_paths_contents_and_failures() {
        let doc = |content: &[u8]| Context {
            docs: vec![ContextDoc {
                path: "docs/a.md".into(),
                origin: ContextOrigin::Link,
                line: 3,
                content: content.to_vec(),
            }],
            references: vec![
                Reference {
                    path: "docs/a.md".into(),
                    origin: ContextOrigin::Link,
                    status: Status::Included(0),
                },
                Reference {
                    path: "docs/gone.md".into(),
                    origin: ContextOrigin::Link,
                    status: Status::Failed(LintCode::ReferenceMissing),
                },
            ],
            issues: Vec::new(),
        };
        let first = pin(doc(b"# A\n"));
        let content = sha256(b"# A\n");
        assert_eq!(
            first.hash,
            sha256(
                format!("link\tdocs/a.md\t{content}\nlink\tdocs/gone.md\treference_missing\n")
                    .as_bytes()
            )
        );
        assert_eq!(
            first.refs,
            vec![ContextRef {
                path: "docs/a.md".into(),
                origin: ContextOrigin::Link,
                line: 3,
                bytes: 4,
                content_hash: content.clone(),
            }]
        );
        assert_eq!(first.blobs, vec![(content, b"# A\n".to_vec())]);
        assert_ne!(pin(doc(b"# B\n")).hash, first.hash);
        assert_eq!(pin(doc(b"# A\n")).hash, first.hash);
    }
}
