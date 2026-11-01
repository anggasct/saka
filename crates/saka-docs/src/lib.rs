use std::sync::Arc;

use async_trait::async_trait;
use saka_ports::{DocFile, DocSource, PortError, PortResult, RepoRef, Repos};
use serde_json::json;

pub struct GitDocs {
    repos: Arc<dyn Repos>,
}

impl GitDocs {
    pub fn new(repos: Arc<dyn Repos>) -> GitDocs {
        GitDocs { repos }
    }

    pub async fn read_at(
        &self,
        repo: &RepoRef,
        commit: &str,
        path: &str,
    ) -> PortResult<Option<DocFile>> {
        Ok(self
            .repos
            .read_file(repo, commit, path)
            .await?
            .map(|bytes| DocFile {
                path: path.to_string(),
                commit: commit.to_string(),
                bytes,
            }))
    }
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
        self.read_at(repo, &commit, path).await
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
}
