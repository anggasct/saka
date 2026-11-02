use saka_core::event::Event;
use saka_core::ids::ProjectId;
use saka_core::paths::{self, MAX_DOC_BYTES, MAX_TREE_FILES};
use saka_core::rejection::{FileUnsupported, Rejection};
use saka_core::vocab::SpecOrigin;
use saka_ports::{DocBlob, DocCommit, DocEntry, DocExpect, DocWrite};
use saka_store::{Batch, read};
use serde_json::{Value, json};

use crate::action::{Action, Produced};
use crate::commands::SpecFile;
use crate::register::{invalid, path_code};
use crate::views::SpecView;
use crate::{Cause, Engine, EngineError, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocTreeView {
    pub commit: String,
    pub base_branch: String,
    pub project_path: String,
    pub files: Vec<DocEntry>,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocContentView {
    pub path: String,
    pub commit: String,
    pub blob: String,
    pub bytes: u64,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Precondition {
    Blob(String),
    Exists,
    Absent,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct WrittenDoc {
    pub commit: String,
    pub blob: String,
    pub bytes: Vec<u8>,
    pub event: Option<Event>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecProblem {
    pub code: String,
    pub params: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DocWriteView {
    pub project_id: ProjectId,
    pub path: String,
    pub commit: String,
    pub blob: String,
    pub bytes: u64,
    pub committed: bool,
    pub created: bool,
    pub spec: Option<SpecView>,
    pub spec_problem: Option<SpecProblem>,
}

impl Engine {
    pub async fn doc_tree(&self, project_id: ProjectId) -> Result<DocTreeView> {
        let (project, repo) = self.workspace_of(project_id)?;
        let tree = self
            .ports
            .docs
            .tree(
                &repo,
                &project.base_branch,
                &project.project_path,
                MAX_TREE_FILES,
            )
            .await?;
        Ok(DocTreeView {
            commit: tree.commit,
            base_branch: project.base_branch,
            project_path: project.project_path,
            files: tree.files,
            truncated: tree.truncated,
        })
    }

    pub async fn read_doc(&self, project_id: ProjectId, path: &str) -> Result<DocContentView> {
        let (project, repo) = self.workspace_of(project_id)?;
        paths::check_doc_path(&project.project_path, path)
            .map_err(|issue| invalid("path", path_code(issue)))?;
        let opened = self
            .ports
            .docs
            .open(&repo, &project.base_branch, path, MAX_DOC_BYTES)
            .await?;
        let unsupported = |reason| EngineError::Rejected(Rejection::FileUnsupported { reason });
        match opened.blob {
            DocBlob::Missing => Err(Rejection::NotFound.into()),
            DocBlob::NotRegular => Err(unsupported(FileUnsupported::NotRegular)),
            DocBlob::TooLarge { bytes, .. } => Err(unsupported(FileUnsupported::TooLarge {
                bytes,
                max_bytes: MAX_DOC_BYTES,
            })),
            DocBlob::Bytes { blob, bytes } => {
                let size = bytes.len() as u64;
                let content =
                    String::from_utf8(bytes).map_err(|_| unsupported(FileUnsupported::NotUtf8))?;
                Ok(DocContentView {
                    path: path.to_string(),
                    commit: opened.commit,
                    blob,
                    bytes: size,
                    content,
                })
            }
        }
    }

    pub async fn write_doc(
        &self,
        project_id: ProjectId,
        path: &str,
        content: String,
        precondition: Precondition,
        cause: &Cause,
        action: Option<&Action<'_>>,
    ) -> Result<DocWriteView> {
        let bytes = content.into_bytes();
        if bytes.len() as u64 > MAX_DOC_BYTES {
            return Err(invalid("content", "too_large"));
        }
        let (project, repo) = self.workspace_of(project_id)?;
        paths::check_doc_path(&project.project_path, path)
            .map_err(|issue| invalid("path", path_code(issue)))?;
        let written = self
            .commit_doc(&project, &repo, path, bytes, precondition)
            .await?;
        let (spec, spec_problem) = self
            .register_saved(
                &project,
                &repo,
                path,
                &written.commit,
                &written.bytes,
                cause,
            )
            .await?;
        let view = DocWriteView {
            project_id,
            path: path.to_string(),
            commit: written.commit.clone(),
            blob: written.blob.clone(),
            bytes: written.bytes.len() as u64,
            committed: written.event.is_some(),
            created: written
                .event
                .as_ref()
                .is_some_and(|e| matches!(e, Event::DocCommitted { created: true, .. })),
            spec,
            spec_problem,
        };
        let _guard = self.locked()?;
        let now = self.now();
        let mut batch = Batch::new(now);
        if let Some(event) = &written.event {
            self.push_event(&mut batch, event, Some(project_id), cause, now);
        }
        if let Some(action) = action {
            let reply = (action.respond)(Produced::Doc(&view));
            batch.action =
                Some(action.record(self, cause, Some(project_id), Some(project_id.0), reply));
        }
        self.commit(&batch)?;
        Ok(view)
    }

    pub(crate) async fn commit_doc(
        &self,
        project: &saka_core::model::Project,
        repo: &saka_ports::RepoRef,
        path: &str,
        bytes: Vec<u8>,
        precondition: Precondition,
    ) -> Result<WrittenDoc> {
        let request = DocCommit {
            repo: repo.clone(),
            branch: project.base_branch.clone(),
            path: path.to_string(),
            bytes,
            expect: match precondition {
                Precondition::Blob(blob) => DocExpect::Blob(blob),
                Precondition::Exists => DocExpect::Exists,
                Precondition::Absent => DocExpect::Absent,
            },
            author: self.cfg.docs_author.clone(),
            at: self.now(),
        };
        let project_id = project.id;
        let outcome = self.ports.docs.commit_file(&request).await?;
        let size = request.bytes.len() as u64;
        let (commit, blob, event) = match outcome {
            DocWrite::Committed {
                commit,
                parent,
                blob,
                created,
                attempts,
            } => {
                tracing::info!(project_id = %project_id, %commit, created, bytes = size, attempts, "docs.file.committed");
                let event = Event::DocCommitted {
                    project_id,
                    path: path.to_string(),
                    commit: commit.clone(),
                    parent,
                    blob: blob.clone(),
                    bytes: size,
                    created,
                };
                (commit, blob, Some(event))
            }
            DocWrite::Unchanged { commit, blob } => {
                tracing::info!(project_id = %project_id, "docs.file.unchanged");
                (commit, blob, None)
            }
            DocWrite::Stale { commit, blob } => {
                return Err(Rejection::FileStale { blob, commit }.into());
            }
            DocWrite::PathConflict { path } => {
                return Err(Rejection::FilePathConflict { path }.into());
            }
            DocWrite::NotRegular => {
                return Err(Rejection::FileUnsupported {
                    reason: FileUnsupported::NotRegular,
                }
                .into());
            }
            DocWrite::Rejected => {
                tracing::warn!(project_id = %project_id, branch = %project.base_branch, "docs.push.rejected");
                return Err(Rejection::FilePushRejected {
                    branch: project.base_branch.clone(),
                }
                .into());
            }
        };
        Ok(WrittenDoc {
            commit,
            blob,
            bytes: request.bytes,
            event,
        })
    }

    async fn register_saved(
        &self,
        project: &saka_core::model::Project,
        repo: &saka_ports::RepoRef,
        path: &str,
        commit: &str,
        bytes: &[u8],
        cause: &Cause,
    ) -> Result<(Option<SpecView>, Option<SpecProblem>)> {
        let text = String::from_utf8_lossy(bytes);
        let known = self
            .store
            .read(|c| read::spec_by_path(c, project.id, path))?
            .is_some();
        if !known && !saka_spec::is_spec(&text) {
            return Ok((None, None));
        }
        let problem = |code: &str, params: Value| {
            Ok((
                None,
                Some(SpecProblem {
                    code: code.to_string(),
                    params,
                }),
            ))
        };
        if let Err(issue) = paths::check_spec_path(&project.project_path, path) {
            return problem(
                "validation_failed",
                json!({ "errors": [{ "field": "path", "code": path_code(issue) }] }),
            );
        }
        let file = SpecFile {
            path,
            source_commit: commit,
            bytes,
            origin: SpecOrigin::Manual,
        };
        match self.register_at(project, repo, file, cause, None).await {
            Ok((view, _)) => Ok((Some(view), None)),
            Err(EngineError::Rejected(r)) => problem(r.code(), r.params()),
            Err(EngineError::Invalid { field, code }) => problem(
                "validation_failed",
                json!({ "errors": [{ "field": field, "code": code }] }),
            ),
            Err(EngineError::Port(e)) => {
                tracing::warn!(code = %e.code, "spec context unavailable after a docs save");
                problem("scm_unavailable", json!({ "provider_status": null }))
            }
            Err(e) => Err(e),
        }
    }
}
