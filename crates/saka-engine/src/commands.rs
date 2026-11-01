//! Commands from the operator (through the API) and the registration steps that put a
//! project and its specs into the store.

use saka_core::Ctx;
use saka_core::event::Event;
use saka_core::ids::{PermissionRequestId, ProjectId, RepoId, SpecId, SpecRevisionId, WorkItemId};
use saka_core::model::ProjectRepo;
use saka_core::rejection::Rejection;
use saka_core::spec::{self, NewRevision};
use saka_core::task::{self, TaskInput};
use saka_core::workspace;
use saka_ports::IdsSource;
use saka_spec::{LintIssue, PARSER_VERSION};
use saka_store::{Batch, SpecRevisionRow, read};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::{Cause, Engine, EngineError, Extra, Mode, OPERATOR, Result};

const REASON_MAX: usize = 500;

/// A project with its one repo, as read from its `saka.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewProject {
    pub name: String,
    pub integration_id: String,
    pub provider: String,
    pub full_name: String,
    pub url: String,
    pub default_branch: String,
    pub project_path: String,
    pub base_branch: String,
    pub config_commit: String,
    pub repo_name: String,
}

/// The result of reading a spec file into a revision.
#[derive(Debug, Clone, PartialEq)]
pub struct RegisteredSpec {
    pub spec_id: SpecId,
    pub revision_id: SpecRevisionId,
    pub lint_ok: bool,
    pub issues: Vec<LintIssue>,
}

fn reason(value: &str) -> Result<String> {
    let trimmed = value.trim();
    let len = trimmed.chars().count();
    if len == 0 || len > REASON_MAX {
        return Err(EngineError::Invalid {
            field: "reason",
            code: "length".into(),
        });
    }
    Ok(trimmed.to_string())
}

fn content_hash(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    format!("sha256:{hex}")
}

impl Engine {
    pub fn register_project(&self, p: &NewProject, cause: &Cause) -> Result<(ProjectId, RepoId)> {
        let _guard = self.locked()?;
        let now = self.now();
        let existing = self
            .store
            .read(|c| read::repo_by_name(c, &p.integration_id, &p.full_name))?;
        let mut batch = Batch::new(now);
        let mut events = Vec::new();
        let repo_id = match existing {
            Some(repo) => repo.id,
            None => {
                let repo_id = RepoId(self.new_id());
                events.push(Event::RepoRegistered {
                    repo_id,
                    integration_id: p.integration_id.clone(),
                    provider: p.provider.clone(),
                    full_name: p.full_name.clone(),
                    url: p.url.clone(),
                    default_branch: p.default_branch.clone(),
                });
                repo_id
            }
        };
        let project_id = ProjectId(self.new_id());
        events.push(Event::ProjectRegistered {
            project_id,
            name: p.name.clone(),
            workspace_repo_id: repo_id,
            project_path: p.project_path.clone(),
            base_branch: p.base_branch.clone(),
            config_commit: p.config_commit.clone(),
            repos: vec![ProjectRepo {
                repo_id,
                name: p.repo_name.clone(),
                is_default: true,
                path_scope: vec!["**".into()],
            }],
        });
        for e in &events {
            if let Some(repo) = workspace::evolve_repo(None, e, now) {
                batch.repos.push(repo);
            }
            if let Some(project) = workspace::evolve_project(None, e, now) {
                batch.projects.push(project);
            }
            self.push_event(&mut batch, e, Some(project_id), cause, now);
        }
        self.commit(&batch)?;
        Ok((project_id, repo_id))
    }

    /// Records the spec file read at `source_commit` as a revision (or finds the revision
    /// with the same content).
    pub fn register_spec(
        &self,
        project_id: ProjectId,
        path: &str,
        source_commit: &str,
        bytes: &[u8],
        cause: &Cause,
    ) -> Result<RegisteredSpec> {
        let _guard = self.locked()?;
        let now = self.now();
        let file_name = path.rsplit('/').next().unwrap_or(path);
        let stem = file_name.strip_suffix(".md").unwrap_or(file_name);
        let parsed = saka_spec::parse(file_name, bytes);
        let hash = content_hash(bytes);
        let (existing, known) = self.store.read(|c| {
            let existing = read::spec_by_path(c, project_id, path)?;
            let known = match &existing {
                Some(s) => read::revision_by_hash(c, s.id, &hash)?,
                None => None,
            };
            Ok((existing, known))
        })?;
        let lint_ok = parsed.is_ready();
        let revision = NewRevision {
            source_commit: source_commit.to_string(),
            content_hash: hash.clone(),
            lint_ok,
            issue_codes: parsed
                .issues
                .iter()
                .map(|i| i.code.as_str().to_string())
                .collect(),
            title: parsed
                .model
                .as_ref()
                .map_or_else(|| stem.to_string(), |m| m.title.clone()),
        };
        let slug = parsed
            .model
            .as_ref()
            .map_or_else(|| stem.to_string(), |m| m.slug.clone());
        let mut ids = IdsSource(&*self.ports.ids);
        let mut ctx = Ctx { now, ids: &mut ids };
        let events = spec::record_revision(
            existing.as_ref(),
            project_id,
            path,
            &slug,
            revision,
            known,
            &mut ctx,
        );

        let mut batch = Batch::new(now);
        let mut after = existing.clone();
        let mut revision_id = existing.as_ref().map(|s| s.latest_revision_id);
        for e in &events {
            after = spec::evolve(after, e, now);
            if let Event::SpecRevisionRecorded {
                spec_id,
                revision_id: id,
                reused,
                ..
            } = e
            {
                revision_id = Some(*id);
                if !reused {
                    batch.revisions.push(SpecRevisionRow {
                        id: id.0,
                        spec_id: spec_id.0,
                        project_id,
                        source_commit: source_commit.to_string(),
                        content_hash: hash.clone(),
                        content: bytes.to_vec(),
                        lint_ok,
                        lint_issues: serde_json::to_value(&parsed.issues)
                            .map_err(|e| EngineError::Internal(e.to_string()))?,
                        model: match &parsed.model {
                            Some(m) => serde_json::to_value(m)
                                .map_err(|e| EngineError::Internal(e.to_string()))?,
                            None => json!({}),
                        },
                        parser_version: PARSER_VERSION,
                        created_at: now,
                    });
                }
            }
            self.push_event(&mut batch, e, Some(project_id), cause, now);
        }
        let spec = after.ok_or_else(|| EngineError::Internal("spec not recorded".into()))?;
        let revision_id = revision_id.ok_or_else(|| EngineError::Internal("no revision".into()))?;
        batch.specs.push(spec.clone());
        self.commit(&batch)?;
        Ok(RegisteredSpec {
            spec_id: spec.id,
            revision_id,
            lint_ok,
            issues: parsed.issues,
        })
    }

    /// Approves the latest revision: one task per step, in one transaction.
    pub fn approve_spec(
        &self,
        spec_id: SpecId,
        if_match: SpecRevisionId,
        reason: Option<String>,
        cause: &Cause,
    ) -> Result<Vec<WorkItemId>> {
        let _guard = self.locked()?;
        let now = self.now();
        let (spec, view) = self.store.read(|c| {
            let spec = read::load_spec(c, spec_id)?;
            let view = match &spec {
                Some(s) => read::revision_view(c, s.latest_revision_id)?,
                None => None,
            };
            Ok((spec, view))
        })?;
        let (Some(spec), Some(view)) = (spec, view) else {
            return Err(Rejection::NotFound.into());
        };
        let project = self
            .store
            .read(|c| read::load_project(c, spec.project_id))?
            .ok_or(EngineError::Rejected(Rejection::NotFound))?;
        let default_repo = project
            .default_repo()
            .ok_or_else(|| EngineError::Internal("project has no default repo".into()))?;
        let mut ids = IdsSource(&*self.ports.ids);
        let mut ctx = Ctx { now, ids: &mut ids };
        let events = spec::approve(&spec, &view, if_match, reason, default_repo, &mut ctx)?;

        let mut batch = Batch::new(now);
        let mut after = Some(spec.clone());
        let mut created = Vec::new();
        for e in &events {
            after = spec::evolve(after, e, now);
            if let Some(t) = task::evolve(None, e, now) {
                created.push(t.item.id);
                batch.tasks.push(t);
            }
            self.push_event(&mut batch, e, Some(spec.project_id), cause, now);
        }
        batch.specs.extend(after);
        self.commit(&batch)?;
        Ok(created)
    }

    pub fn retry(&self, id: WorkItemId, why: &str, cause: &Cause) -> Result<Vec<Event>> {
        let reason = reason(why)?;
        self.decide(
            id,
            Some(TaskInput::Retry { reason }),
            Mode::Command,
            cause,
            Extra::default(),
        )
    }

    pub fn cancel(&self, id: WorkItemId, why: &str, cause: &Cause) -> Result<Vec<Event>> {
        let reason = reason(why)?;
        self.decide(
            id,
            Some(TaskInput::Cancel { reason }),
            Mode::Command,
            cause,
            Extra::default(),
        )
    }

    pub fn request_merge(
        &self,
        id: WorkItemId,
        expected_version: u64,
        why: Option<&str>,
        cause: &Cause,
    ) -> Result<Vec<Event>> {
        let reason = why.map(reason).transpose()?;
        self.decide(
            id,
            Some(TaskInput::RequestMerge {
                expected_version,
                reason,
            }),
            Mode::Command,
            cause,
            Extra::default(),
        )
    }

    pub fn answer_permission(
        &self,
        request: PermissionRequestId,
        allow: bool,
        message: Option<String>,
        cause: &Cause,
    ) -> Result<Vec<Event>> {
        let work_item = self
            .store
            .read(|c| read::permission_work_item(c, request))?
            .ok_or(EngineError::Rejected(Rejection::NotFound))?;
        let input = TaskInput::AnswerPermission {
            permission_request_id: request,
            allow,
            message,
            actor: OPERATOR.to_string(),
        };
        self.decide(
            work_item,
            Some(input),
            Mode::Command,
            cause,
            Extra::default(),
        )
    }
}
