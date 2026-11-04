use std::sync::Arc;

use async_trait::async_trait;
use saka_core::paths::{is_doc_file_name, is_inside_project};
use saka_ports::{
    Blob, CommitOne, CommitPush, DocBlob, DocCommit, DocEntry, DocExpect, DocFile, DocOpened,
    DocSource, DocTree, DocWrite, EntryKind, PortError, PortResult, PushCommit, RepoRef, Repos,
    TreeEntry,
};
use serde_json::json;

const ALWAYS_PATTERN: &str = "^context:[[:space:]]*always[[:space:]]*$";
const DOC_GLOBS: [&str; 2] = ["*.md", "*.markdown"];
const COMMIT_ATTEMPTS: u32 = 3;

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

impl GitDocs {
    async fn fetched_tip(&self, repo: &RepoRef, branch: &str) -> PortResult<String> {
        self.repos.fetch(repo).await?;
        self.repos.tip(repo, branch).await?.ok_or_else(|| {
            PortError::permanent("base_branch_missing", json!({ "base_branch": branch }))
        })
    }

    async fn attempt(&self, request: &DocCommit, tip: String, blob: &str) -> PortResult<Attempt> {
        let wanted = ancestors_and_self(&request.path);
        let entries = self.repos.entries(&request.repo, &tip, &wanted).await?;
        let find = |path: &str| entries.iter().find(|e| e.path == path);
        for ancestor in &wanted[..wanted.len() - 1] {
            if find(ancestor).is_some_and(|e| e.kind != EntryKind::Tree) {
                return Ok(Attempt::Done(DocWrite::PathConflict {
                    path: ancestor.clone(),
                }));
            }
        }
        let current = find(&request.path);
        match current {
            Some(e) if e.kind == EntryKind::Tree => {
                return Ok(Attempt::Done(DocWrite::PathConflict {
                    path: request.path.clone(),
                }));
            }
            Some(e) if !e.is_regular_file() => return Ok(Attempt::Done(DocWrite::NotRegular)),
            Some(e) if e.object == blob => {
                return Ok(Attempt::Done(DocWrite::Unchanged {
                    commit: tip,
                    blob: blob.to_string(),
                }));
            }
            _ => {}
        }
        let current_blob = current.map(|e| e.object.clone());
        let holds = match &request.expect {
            DocExpect::Absent => current.is_none(),
            DocExpect::Exists => current.is_some(),
            DocExpect::Blob(expected) => current_blob.as_deref() == Some(expected.as_str()),
        };
        if !holds {
            return Ok(Attempt::Done(DocWrite::Stale {
                commit: tip,
                blob: current_blob,
            }));
        }
        let created = current.is_none();
        let mode = current.map_or(TreeEntry::REGULAR, |e| e.mode.as_str());
        let verb = if created { "add" } else { "update" };
        let commit = self
            .repos
            .commit_one(&CommitOne {
                repo: request.repo.clone(),
                parent: tip.clone(),
                path: request.path.clone(),
                mode: mode.to_string(),
                blob: blob.to_string(),
                message: format!("docs: {verb} {}", request.path),
                author: request.author.clone(),
                at: request.at,
            })
            .await?;
        let pushed = self
            .repos
            .push_commit(&PushCommit {
                repo: request.repo.clone(),
                branch: request.branch.clone(),
                commit: commit.clone(),
                expected: tip.clone(),
            })
            .await?;
        Ok(match pushed {
            CommitPush::Pushed => Attempt::Done(DocWrite::Committed {
                commit,
                parent: tip,
                blob: blob.to_string(),
                created,
                attempts: 0,
            }),
            CommitPush::Moved { .. } => Attempt::Moved,
            CommitPush::Rejected => Attempt::Done(DocWrite::Rejected),
        })
    }
}

enum Attempt {
    Done(DocWrite),
    Moved,
}

fn ancestors_and_self(path: &str) -> Vec<String> {
    let mut out: Vec<String> = path
        .match_indices('/')
        .map(|(i, _)| path[..i].to_string())
        .collect();
    out.push(path.to_string());
    out
}

#[async_trait]
impl DocSource for GitDocs {
    async fn branch_tip(&self, repo: &RepoRef, branch: &str) -> PortResult<Option<String>> {
        self.repos.fetch(repo).await?;
        self.repos.tip(repo, branch).await
    }

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

    async fn tree(
        &self,
        repo: &RepoRef,
        branch: &str,
        project_path: &str,
        max_files: usize,
    ) -> PortResult<DocTree> {
        let commit = self.fetched_tip(repo, branch).await?;
        let mut files = self.tree_at(repo, &commit, project_path).await?;
        let truncated = files.len() > max_files;
        files.truncate(max_files);
        Ok(DocTree {
            commit,
            files,
            truncated,
        })
    }

    async fn tree_at(
        &self,
        repo: &RepoRef,
        commit: &str,
        project_path: &str,
    ) -> PortResult<Vec<DocEntry>> {
        let mut files: Vec<DocEntry> = self
            .repos
            .list_tree(repo, commit, project_path)
            .await?
            .into_iter()
            .filter(|e| {
                e.is_regular_file()
                    && is_inside_project(project_path, &e.path)
                    && is_doc_file_name(&e.path)
            })
            .map(|e| DocEntry {
                bytes: e.bytes.unwrap_or_default(),
                path: e.path,
                blob: e.object,
            })
            .collect();
        files.sort_by(|a, b| a.path.as_bytes().cmp(b.path.as_bytes()));
        Ok(files)
    }

    async fn open(
        &self,
        repo: &RepoRef,
        branch: &str,
        path: &str,
        max_bytes: u64,
    ) -> PortResult<DocOpened> {
        let commit = self.fetched_tip(repo, branch).await?;
        let entries = self
            .repos
            .entries(repo, &commit, &[path.to_string()])
            .await?;
        let blob = match entries.into_iter().find(|e| e.path == path) {
            None => DocBlob::Missing,
            Some(e) if e.kind == EntryKind::Tree => DocBlob::Missing,
            Some(e) if !e.is_regular_file() => DocBlob::NotRegular,
            Some(e) if e.bytes.unwrap_or_default() > max_bytes => DocBlob::TooLarge {
                bytes: e.bytes.unwrap_or_default(),
                blob: e.object,
            },
            Some(e) => match self.repos.read_file(repo, &commit, path).await? {
                Some(bytes) => DocBlob::Bytes {
                    blob: e.object,
                    bytes,
                },
                None => DocBlob::Missing,
            },
        };
        Ok(DocOpened { commit, blob })
    }

    async fn commit_file(&self, request: &DocCommit) -> PortResult<DocWrite> {
        let mut blob = None;
        for attempt in 1..=COMMIT_ATTEMPTS {
            let tip = self.fetched_tip(&request.repo, &request.branch).await?;
            let blob = match &blob {
                Some(blob) => blob,
                None => blob.insert(self.repos.write_blob(&request.repo, &request.bytes).await?),
            };
            match self.attempt(request, tip, blob).await? {
                Attempt::Done(DocWrite::Committed {
                    commit,
                    parent,
                    blob,
                    created,
                    ..
                }) => {
                    return Ok(DocWrite::Committed {
                        commit,
                        parent,
                        blob,
                        created,
                        attempts: attempt,
                    });
                }
                Attempt::Done(outcome) => return Ok(outcome),
                Attempt::Moved => {}
            }
        }
        Err(PortError::retryable(
            "branch_moving",
            json!({ "branch": request.branch, "attempts": COMMIT_ATTEMPTS }),
        ))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, VecDeque};
    use std::sync::Mutex;

    use saka_core::authorship::GitIdentity;
    use saka_core::ids::RepoId;
    use saka_core::time::Timestamp;
    use saka_ports::{PushOutcome, PushRequest};
    use uuid::Uuid;

    use super::*;

    #[derive(Default)]
    struct Mirror {
        fetches: Mutex<u32>,
        greps: Mutex<Vec<(String, Vec<String>)>>,
        tips: Mutex<HashMap<String, String>>,
        files: HashMap<(String, String), Vec<u8>>,
        trees: HashMap<String, Vec<TreeEntry>>,
        pushes: Mutex<VecDeque<CommitPush>>,
        commits: Mutex<Vec<CommitOne>>,
    }

    #[async_trait]
    impl Repos for Mirror {
        async fn fetch(&self, _repo: &RepoRef) -> PortResult<()> {
            *self.fetches.lock().unwrap() += 1;
            Ok(())
        }

        async fn tip(&self, _repo: &RepoRef, branch: &str) -> PortResult<Option<String>> {
            Ok(self.tips.lock().unwrap().get(branch).cloned())
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

        async fn list_tree(
            &self,
            _repo: &RepoRef,
            commit: &str,
            _dir: &str,
        ) -> PortResult<Vec<TreeEntry>> {
            Ok(self.trees.get(commit).cloned().unwrap_or_default())
        }

        async fn entries(
            &self,
            _repo: &RepoRef,
            commit: &str,
            paths: &[String],
        ) -> PortResult<Vec<TreeEntry>> {
            Ok(self
                .trees
                .get(commit)
                .into_iter()
                .flatten()
                .filter(|e| paths.contains(&e.path))
                .cloned()
                .collect())
        }

        async fn write_blob(&self, _repo: &RepoRef, bytes: &[u8]) -> PortResult<String> {
            Ok(blob_of(bytes))
        }

        async fn commit_one(&self, request: &CommitOne) -> PortResult<String> {
            let mut commits = self.commits.lock().unwrap();
            commits.push(request.clone());
            Ok(format!("{:040x}", 0xc0 + commits.len()))
        }

        async fn push_commit(&self, request: &PushCommit) -> PortResult<CommitPush> {
            let outcome = self
                .pushes
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(CommitPush::Pushed);
            if let CommitPush::Moved {
                remote_sha: Some(sha),
            } = &outcome
            {
                self.tips
                    .lock()
                    .unwrap()
                    .insert(request.branch.clone(), sha.clone());
            }
            Ok(outcome)
        }
    }

    fn blob_of(bytes: &[u8]) -> String {
        format!("{:040x}", bytes.len() + 0xb0)
    }

    fn repo() -> RepoRef {
        RepoRef {
            repo_id: RepoId(Uuid::from_u128(1)),
            integration_id: "github".into(),
            full_name: "acme/app".into(),
            url: "https://github.com/acme/app".into(),
        }
    }

    fn entry(path: &str, mode: &str, kind: EntryKind, object: &str, bytes: u64) -> TreeEntry {
        TreeEntry {
            path: path.into(),
            mode: mode.into(),
            kind,
            object: object.into(),
            bytes: (kind == EntryKind::Blob).then_some(bytes),
        }
    }

    fn file(path: &str, object: &str) -> TreeEntry {
        entry(path, TreeEntry::REGULAR, EntryKind::Blob, object, 10)
    }

    fn dir(path: &str) -> TreeEntry {
        entry(path, "040000", EntryKind::Tree, &"d".repeat(40), 0)
    }

    const TIP: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const NEXT: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const OLD: &str = "1111111111111111111111111111111111111111";

    fn mirror(trees: Vec<(&str, Vec<TreeEntry>)>) -> Mirror {
        let mirror = Mirror {
            trees: trees.into_iter().map(|(c, t)| (c.to_string(), t)).collect(),
            ..Mirror::default()
        };
        mirror
            .tips
            .lock()
            .unwrap()
            .insert("main".into(), TIP.into());
        mirror
    }

    fn write(path: &str, bytes: &[u8], expect: DocExpect) -> DocCommit {
        DocCommit {
            repo: repo(),
            branch: "main".into(),
            path: path.into(),
            bytes: bytes.to_vec(),
            expect,
            author: GitIdentity {
                name: "Saka Agent".into(),
                email: "agent@saka.invalid".into(),
            },
            at: Timestamp(1_800_000_000_123),
        }
    }

    #[tokio::test]
    async fn the_tree_lists_regular_markdown_files_inside_the_project_sorted_by_bytes() {
        let tree = vec![
            file("docs/z.md", OLD),
            file("docs/specs/b.MD", OLD),
            file("docs/A.markdown", OLD),
            file("docs/c.txt", OLD),
            file("docsx/d.md", OLD),
            file("src/e.md", OLD),
            entry("docs/link.md", "120000", EntryKind::Blob, OLD, 9),
            entry("docs/sub", "160000", EntryKind::Commit, OLD, 0),
            entry(
                "docs/run.md",
                TreeEntry::EXECUTABLE,
                EntryKind::Blob,
                OLD,
                3,
            ),
        ];
        let docs = GitDocs::new(Arc::new(mirror(vec![(TIP, tree)])));
        let listed = docs.tree(&repo(), "main", "docs", 10).await.unwrap();
        let paths: Vec<&str> = listed.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(
            paths,
            vec![
                "docs/A.markdown",
                "docs/run.md",
                "docs/specs/b.MD",
                "docs/z.md"
            ]
        );
        assert_eq!(listed.commit, TIP);
        assert!(!listed.truncated);
        assert_eq!(listed.files[1].bytes, 3);
        let cut = docs.tree(&repo(), "main", "docs", 2).await.unwrap();
        assert_eq!(cut.files.len(), 2);
        assert!(cut.truncated);
        let all = docs.tree(&repo(), "main", ".", 10).await.unwrap();
        assert!(all.files.iter().any(|f| f.path == "src/e.md"));
        let at = docs.tree_at(&repo(), TIP, "docs").await.unwrap();
        assert_eq!(at, listed.files);
    }

    #[tokio::test]
    async fn opening_a_file_tells_missing_folders_links_and_large_files_apart() {
        let tree = vec![
            file("docs/a.md", OLD),
            dir("docs/specs"),
            entry("docs/link.md", "120000", EntryKind::Blob, OLD, 9),
            entry("docs/big.md", TreeEntry::REGULAR, EntryKind::Blob, NEXT, 99),
        ];
        let mut m = mirror(vec![(TIP, tree)]);
        m.files
            .insert((TIP.into(), "docs/a.md".into()), b"# A\r\n".to_vec());
        let docs = GitDocs::new(Arc::new(m));
        let repo = repo();
        let open = |p: &'static str| docs.open(&repo, "main", p, 50);
        assert_eq!(
            open("docs/a.md").await.unwrap(),
            DocOpened {
                commit: TIP.into(),
                blob: DocBlob::Bytes {
                    blob: OLD.into(),
                    bytes: b"# A\r\n".to_vec()
                }
            }
        );
        assert_eq!(open("docs/none.md").await.unwrap().blob, DocBlob::Missing);
        assert_eq!(open("docs/specs").await.unwrap().blob, DocBlob::Missing);
        assert_eq!(
            open("docs/link.md").await.unwrap().blob,
            DocBlob::NotRegular
        );
        assert_eq!(
            open("docs/big.md").await.unwrap().blob,
            DocBlob::TooLarge {
                blob: NEXT.into(),
                bytes: 99
            }
        );
    }

    #[tokio::test]
    async fn a_new_file_becomes_one_commit_on_the_tip_with_an_add_message() {
        let m = Arc::new(mirror(vec![(TIP, vec![dir("docs")])]));
        let docs = GitDocs::new(m.clone());
        let out = docs
            .commit_file(&write("docs/specs/new.md", b"x", DocExpect::Absent))
            .await
            .unwrap();
        assert_eq!(
            out,
            DocWrite::Committed {
                commit: format!("{:040x}", 0xc1),
                parent: TIP.into(),
                blob: blob_of(b"x"),
                created: true,
                attempts: 1,
            }
        );
        let commits = m.commits.lock().unwrap();
        assert_eq!(commits[0].message, "docs: add docs/specs/new.md");
        assert_eq!(commits[0].mode, TreeEntry::REGULAR);
        assert_eq!(commits[0].parent, TIP);
        assert_eq!(commits[0].author.name, "Saka Agent");
    }

    #[tokio::test]
    async fn an_update_keeps_the_file_mode_and_says_update() {
        let tree = vec![entry(
            "docs/run.md",
            TreeEntry::EXECUTABLE,
            EntryKind::Blob,
            OLD,
            3,
        )];
        let m = Arc::new(mirror(vec![(TIP, tree)]));
        let docs = GitDocs::new(m.clone());
        let out = docs
            .commit_file(&write("docs/run.md", b"new", DocExpect::Blob(OLD.into())))
            .await
            .unwrap();
        assert!(matches!(out, DocWrite::Committed { created: false, .. }));
        let commits = m.commits.lock().unwrap();
        assert_eq!(commits[0].mode, TreeEntry::EXECUTABLE);
        assert_eq!(commits[0].message, "docs: update docs/run.md");
    }

    #[tokio::test]
    async fn the_same_content_is_unchanged_even_with_a_stale_precondition() {
        let same = blob_of(b"same");
        let m = Arc::new(mirror(vec![(TIP, vec![file("docs/a.md", &same)])]));
        let docs = GitDocs::new(m.clone());
        for expect in [DocExpect::Absent, DocExpect::Blob(OLD.into())] {
            let out = docs
                .commit_file(&write("docs/a.md", b"same", expect))
                .await
                .unwrap();
            assert_eq!(
                out,
                DocWrite::Unchanged {
                    commit: TIP.into(),
                    blob: same.clone()
                }
            );
        }
        assert!(m.commits.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_precondition_that_does_not_hold_is_stale_with_the_current_blob() {
        let m = Arc::new(mirror(vec![(TIP, vec![file("docs/a.md", OLD)])]));
        let docs = GitDocs::new(m.clone());
        let stale = |blob: Option<&str>| DocWrite::Stale {
            commit: TIP.into(),
            blob: blob.map(str::to_string),
        };
        let cases = [
            ("docs/a.md", DocExpect::Blob(NEXT.into()), stale(Some(OLD))),
            ("docs/a.md", DocExpect::Absent, stale(Some(OLD))),
            ("docs/b.md", DocExpect::Blob(OLD.into()), stale(None)),
            ("docs/b.md", DocExpect::Exists, stale(None)),
        ];
        for (path, expect, wanted) in cases {
            let out = docs
                .commit_file(&write(path, b"new", expect))
                .await
                .unwrap();
            assert_eq!(out, wanted, "{path}");
        }
        assert!(m.commits.lock().unwrap().is_empty());
        let out = docs
            .commit_file(&write("docs/a.md", b"new", DocExpect::Exists))
            .await
            .unwrap();
        assert!(matches!(out, DocWrite::Committed { .. }));
    }

    #[tokio::test]
    async fn a_file_in_the_way_of_the_path_is_a_conflict() {
        let tree = vec![
            file("docs/a.md", OLD),
            dir("docs/specs"),
            entry("docs/link.md", "120000", EntryKind::Blob, OLD, 9),
        ];
        let docs = GitDocs::new(Arc::new(mirror(vec![(TIP, tree)])));
        let out = docs
            .commit_file(&write("docs/a.md/b.md", b"x", DocExpect::Absent))
            .await
            .unwrap();
        assert_eq!(
            out,
            DocWrite::PathConflict {
                path: "docs/a.md".into()
            }
        );
        let out = docs
            .commit_file(&write("docs/specs", b"x", DocExpect::Absent))
            .await
            .unwrap();
        assert_eq!(
            out,
            DocWrite::PathConflict {
                path: "docs/specs".into()
            }
        );
        let out = docs
            .commit_file(&write("docs/link.md", b"x", DocExpect::Exists))
            .await
            .unwrap();
        assert_eq!(out, DocWrite::NotRegular);
    }

    #[tokio::test]
    async fn a_moved_branch_is_retried_on_the_new_tip_and_a_refusal_is_reported() {
        let m = Arc::new(mirror(vec![
            (TIP, vec![file("docs/a.md", OLD)]),
            (NEXT, vec![file("docs/a.md", OLD), file("docs/b.md", OLD)]),
        ]));
        m.pushes.lock().unwrap().push_back(CommitPush::Moved {
            remote_sha: Some(NEXT.into()),
        });
        let docs = GitDocs::new(m.clone());
        let out = docs
            .commit_file(&write("docs/a.md", b"mine", DocExpect::Blob(OLD.into())))
            .await
            .unwrap();
        assert!(
            matches!(&out, DocWrite::Committed { parent, attempts: 2, .. } if parent == NEXT),
            "{out:?}"
        );
        assert_eq!(*m.fetches.lock().unwrap(), 2);

        m.pushes.lock().unwrap().push_back(CommitPush::Rejected);
        let out = docs
            .commit_file(&write("docs/a.md", b"again", DocExpect::Blob(OLD.into())))
            .await
            .unwrap();
        assert_eq!(out, DocWrite::Rejected);

        for _ in 0..3 {
            m.pushes.lock().unwrap().push_back(CommitPush::Moved {
                remote_sha: Some(NEXT.into()),
            });
        }
        let err = docs
            .commit_file(&write("docs/a.md", b"busy", DocExpect::Blob(OLD.into())))
            .await
            .unwrap_err();
        assert_eq!(err.code, "branch_moving");
        assert!(err.retryable);
    }

    #[tokio::test]
    async fn files_are_read_at_the_fetched_tip_of_the_base_branch() {
        let commit = "a".repeat(40);
        let mut mirror = Mirror::default();
        mirror
            .tips
            .lock()
            .unwrap()
            .insert("main".into(), commit.clone());
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
