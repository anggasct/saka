use std::sync::Arc;

use async_trait::async_trait;
use saka_ports::{Blob, DocFile, DocSource, PortError, PortResult, RepoRef, Repos};
use serde_json::json;

const ALWAYS_PATTERN: &str = "^context:[[:space:]]*always[[:space:]]*$";
const DOC_GLOBS: [&str; 2] = ["*.md", "*.markdown"];

pub struct GitDocs {
    repos: Arc<dyn Repos>,
}

impl GitDocs {
    pub fn new(repos: Arc<dyn Repos>) -> GitDocs {
        GitDocs { repos }
    }
}

fn doc_pathspecs(project_path: &str) -> Vec<String> {
    let dir = project_path.trim_matches('/');
    DOC_GLOBS
        .iter()
        .map(|glob| match dir {
            "" | "." => format!(":(glob)**/{glob}"),
            dir => format!(":(glob){dir}/**/{glob}"),
        })
        .collect()
}

#[async_trait]
impl DocSource for GitDocs {
    async fn read_at_tip(
        &self,
        repo: &RepoRef,
        branch: &str,
        path: &str,
    ) -> PortResult<Option<DocFile>> {
        self.repos.fetch(repo).await?;
        let commit = self.repos.tip(repo, branch).await?.ok_or_else(|| {
            PortError::permanent("base_branch_missing", json!({ "base_branch": branch }))
        })?;
        Ok(self
            .repos
            .read_file(repo, &commit, path)
            .await?
            .map(|bytes| DocFile {
                path: path.to_string(),
                commit: commit.clone(),
                bytes,
            }))
    }

    async fn read_at(
        &self,
        repo: &RepoRef,
        commit: &str,
        path: &str,
        max_bytes: u64,
    ) -> PortResult<Blob> {
        self.repos.read_bounded(repo, commit, path, max_bytes).await
    }

    async fn find_always(
        &self,
        repo: &RepoRef,
        commit: &str,
        project_path: &str,
    ) -> PortResult<Vec<String>> {
        let mut paths = self
            .repos
            .grep_paths(repo, commit, ALWAYS_PATTERN, &doc_pathspecs(project_path))
            .await?;
        paths.sort();
        paths.dedup();
        Ok(paths)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use saka_core::ids::RepoId;
    use saka_ports::{PushOutcome, PushRequest};
    use uuid::Uuid;

    use super::*;

    #[derive(Default)]
    struct Mirror {
        fetches: Mutex<u32>,
        greps: Mutex<Vec<(String, Vec<String>)>>,
        tips: HashMap<String, String>,
        files: HashMap<(String, String), Vec<u8>>,
    }

    #[async_trait]
    impl Repos for Mirror {
        async fn fetch(&self, _repo: &RepoRef) -> PortResult<()> {
            *self.fetches.lock().unwrap() += 1;
            Ok(())
        }

        async fn tip(&self, _repo: &RepoRef, branch: &str) -> PortResult<Option<String>> {
            Ok(self.tips.get(branch).cloned())
        }

        async fn read_file(
            &self,
            _repo: &RepoRef,
            commit: &str,
            path: &str,
        ) -> PortResult<Option<Vec<u8>>> {
            Ok(self
                .files
                .get(&(commit.to_string(), path.to_string()))
                .cloned())
        }

        async fn read_bounded(
            &self,
            _repo: &RepoRef,
            commit: &str,
            path: &str,
            max_bytes: u64,
        ) -> PortResult<Blob> {
            Ok(
                match self.files.get(&(commit.to_string(), path.to_string())) {
                    None => Blob::Missing,
                    Some(b) if b.len() as u64 > max_bytes => Blob::TooLarge {
                        bytes: b.len() as u64,
                    },
                    Some(b) => Blob::Bytes(b.clone()),
                },
            )
        }

        async fn grep_paths(
            &self,
            _repo: &RepoRef,
            commit: &str,
            pattern: &str,
            pathspecs: &[String],
        ) -> PortResult<Vec<String>> {
            self.greps
                .lock()
                .unwrap()
                .push((pattern.to_string(), pathspecs.to_vec()));
            Ok(self
                .files
                .keys()
                .filter(|(c, _)| c == commit)
                .map(|(_, p)| p.clone())
                .collect())
        }

        async fn push(&self, _request: &PushRequest) -> PortResult<PushOutcome> {
            Err(PortError::permanent("unused", json!({})))
        }
    }

    #[tokio::test]
    async fn files_are_read_at_the_fetched_tip_of_the_base_branch() {
        let commit = "a".repeat(40);
        let mut mirror = Mirror::default();
        mirror.tips.insert("main".into(), commit.clone());
        mirror.files.insert(
            (commit.clone(), "docs/specs/a.md".into()),
            b"# A\r\n".to_vec(),
        );
        let mirror = Arc::new(mirror);
        let docs = GitDocs::new(mirror.clone());
        let repo = RepoRef {
            repo_id: RepoId(Uuid::from_u128(1)),
            integration_id: "github".into(),
            full_name: "acme/app".into(),
            url: "https://github.com/acme/app".into(),
        };

        let file = docs
            .read_at_tip(&repo, "main", "docs/specs/a.md")
            .await
            .unwrap();
        assert_eq!(
            file,
            Some(DocFile {
                path: "docs/specs/a.md".into(),
                commit: commit.clone(),
                bytes: b"# A\r\n".to_vec(),
            })
        );
        assert_eq!(*mirror.fetches.lock().unwrap(), 1);
        assert_eq!(
            docs.read_at_tip(&repo, "main", "docs/b.md").await.unwrap(),
            None
        );
        let missing = docs
            .read_at_tip(&repo, "dev", "docs/a.md")
            .await
            .unwrap_err();
        assert_eq!(missing.code, "base_branch_missing");
        assert!(!missing.retryable);
    }

    #[tokio::test]
    async fn context_reads_are_bounded_and_always_docs_come_back_sorted() {
        let commit = "b".repeat(40);
        let mut mirror = Mirror::default();
        for (path, bytes) in [
            ("docs/z.md", b"---\ncontext: always\n---\n".to_vec()),
            ("docs/a.md", b"---\ncontext: always\n---\n".to_vec()),
            ("docs/big.md", vec![b'x'; 9]),
        ] {
            mirror.files.insert((commit.clone(), path.into()), bytes);
        }
        let mirror = Arc::new(mirror);
        let docs = GitDocs::new(mirror.clone());
        let repo = RepoRef {
            repo_id: RepoId(Uuid::from_u128(1)),
            integration_id: "github".into(),
            full_name: "acme/app".into(),
            url: "https://github.com/acme/app".into(),
        };

        assert_eq!(
            docs.read_at(&repo, &commit, "docs/big.md", 8)
                .await
                .unwrap(),
            Blob::TooLarge { bytes: 9 }
        );
        assert_eq!(
            docs.read_at(&repo, &commit, "docs/a.md", 64).await.unwrap(),
            Blob::Bytes(b"---\ncontext: always\n---\n".to_vec())
        );
        assert_eq!(
            docs.read_at(&repo, &commit, "docs/none.md", 64)
                .await
                .unwrap(),
            Blob::Missing
        );
        let found = docs.find_always(&repo, &commit, "docs").await.unwrap();
        assert_eq!(found, vec!["docs/a.md", "docs/big.md", "docs/z.md"]);
        let greps = mirror.greps.lock().unwrap().clone();
        assert_eq!(
            greps,
            vec![(
                ALWAYS_PATTERN.to_string(),
                vec![
                    ":(glob)docs/**/*.md".to_string(),
                    ":(glob)docs/**/*.markdown".to_string()
                ]
            )]
        );
    }
}

#[cfg(test)]
mod context_tests {
    use super::*;

    #[test]
    fn always_docs_are_searched_under_the_project_folder() {
        assert_eq!(
            doc_pathspecs("docs/app/"),
            vec![":(glob)docs/app/**/*.md", ":(glob)docs/app/**/*.markdown"]
        );
        assert_eq!(
            doc_pathspecs("."),
            vec![":(glob)**/*.md", ":(glob)**/*.markdown"]
        );
    }
}
