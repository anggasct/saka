//! The workspace docs source over git: files read byte-exact at the tip of the base
//! branch, as just fetched, so a revision can be pinned to the commit it came from.

use std::sync::Arc;

use saka_ports::{PortError, PortResult, RepoRef, Repos};
use serde_json::json;

/// A file as read at a commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocFile {
    pub path: String,
    pub commit: String,
    pub bytes: Vec<u8>,
}

/// Why a path is not a workspace path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathIssue {
    /// Empty, absolute, with an empty, `.` or `..` segment, or with a character outside
    /// `A-Za-z0-9._-`.
    Invalid,
    /// Not inside the project folder.
    OutsideProject,
    /// Not a markdown file.
    NotMarkdown,
}

fn segments_ok(path: &str) -> bool {
    !path.is_empty()
        && path.split('/').all(|seg| {
            !seg.is_empty()
                && seg != "."
                && seg != ".."
                && seg
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        })
}

/// Checks a project folder: `.` for the repo root, or a relative path.
pub fn check_project_path(project_path: &str) -> Result<(), PathIssue> {
    if project_path == "." || segments_ok(project_path) {
        Ok(())
    } else {
        Err(PathIssue::Invalid)
    }
}

/// Checks a spec path: relative to the repo root, inside `project_path`, ending in `.md`.
pub fn check_spec_path(project_path: &str, path: &str) -> Result<(), PathIssue> {
    check_project_path(project_path)?;
    if !segments_ok(path) {
        return Err(PathIssue::Invalid);
    }
    if project_path != "." && !path.starts_with(&format!("{project_path}/")) {
        return Err(PathIssue::OutsideProject);
    }
    if !path.ends_with(".md") {
        return Err(PathIssue::NotMarkdown);
    }
    Ok(())
}

/// `saka.toml` of a project folder.
pub fn project_config_path(project_path: &str) -> String {
    if project_path == "." {
        "saka.toml".to_string()
    } else {
        format!("{project_path}/saka.toml")
    }
}

pub struct GitDocs {
    repos: Arc<dyn Repos>,
}

impl GitDocs {
    pub fn new(repos: Arc<dyn Repos>) -> GitDocs {
        GitDocs { repos }
    }

    /// Fetches, then reads `path` at the tip of `origin/<branch>`. `Ok(None)` when the
    /// branch has the commit but not the file.
    pub async fn read_at_tip(
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

    /// Reads `path` at `commit` from the mirror, without fetching.
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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use async_trait::async_trait;
    use saka_core::ids::RepoId;
    use saka_ports::{PushOutcome, PushRequest};
    use uuid::Uuid;

    use super::*;

    #[test]
    fn spec_paths_stay_inside_the_project_folder() {
        assert_eq!(check_spec_path("docs", "docs/specs/a.md"), Ok(()));
        assert_eq!(check_spec_path(".", "specs/a.md"), Ok(()));
        assert_eq!(
            check_spec_path("docs", "src/a.md"),
            Err(PathIssue::OutsideProject)
        );
        assert_eq!(
            check_spec_path("docs", "docsx/a.md"),
            Err(PathIssue::OutsideProject)
        );
        assert_eq!(
            check_spec_path("docs", "docs/a.txt"),
            Err(PathIssue::NotMarkdown)
        );
        for bad in [
            "",
            "/docs/a.md",
            "docs/../a.md",
            "docs//a.md",
            "docs/./a.md",
            "docs/a b.md",
        ] {
            assert_eq!(
                check_spec_path("docs", bad),
                Err(PathIssue::Invalid),
                "{bad}"
            );
        }
        assert_eq!(check_project_path("docs/../x"), Err(PathIssue::Invalid));
        assert_eq!(project_config_path("."), "saka.toml");
        assert_eq!(project_config_path("docs"), "docs/saka.toml");
    }

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
