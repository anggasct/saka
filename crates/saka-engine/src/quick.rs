use saka_core::ids::ProjectId;
use saka_core::paths;
use saka_core::rejection::Rejection;
use saka_core::vocab::SpecOrigin;
use saka_spec::{QuickSpec, check_quick, parse, quick_keeps_structure, quick_slug, render_quick};
use saka_store::{Batch, list};
use serde_json::json;

use crate::action::{Action, Produced};
use crate::commands::SpecFile;
use crate::docs::Precondition;
use crate::register::{invalid, path_code};
use crate::views::{SpecView, TaskView};
use crate::{Cause, Engine, EngineError, Result};

const TASK_PAGE: u32 = 200;
const QUICK_REASON: &str = "quick task";

#[derive(Debug, Clone, PartialEq)]
pub struct QuickRequest {
    pub spec: QuickSpec,
    pub path: Option<String>,
    pub approve: bool,
    pub origin: SpecOrigin,
}

#[derive(Debug, Clone, PartialEq)]
pub struct QuickView {
    pub spec: SpecView,
    pub path: String,
    pub commit_sha: String,
    pub tasks: Vec<TaskView>,
}

fn spec_summary(view: &SpecView) -> serde_json::Value {
    json!({
        "id": view.spec.id,
        "path": view.spec.path,
        "slug": view.spec.slug,
        "state": view.spec.state,
        "revision_id": view.revision.id,
    })
}

impl Engine {
    pub async fn quick_spec_with(
        &self,
        project_id: ProjectId,
        request: QuickRequest,
        cause: &Cause,
        action: Option<&Action<'_>>,
    ) -> Result<QuickView> {
        let outcome = self.run_quick_spec(project_id, &request, cause).await;
        let counted = match &outcome {
            Ok(_) if request.approve => "approved",
            Ok(_) => "created",
            Err(EngineError::Rejected(Rejection::QuickSpecNotApproved { .. })) => "not_approved",
            Err(_) => "refused",
        };
        self.counters
            .quick_spec(request.spec.kind.as_str(), counted);
        let view = outcome?;
        if let Some(action) = action {
            let reply = (action.respond)(Produced::Quick(&view));
            let mut batch = Batch::new(self.now());
            batch.action = Some(action.record(
                self,
                cause,
                Some(project_id),
                Some(view.spec.spec.id.0),
                reply,
            ));
            self.commit(&batch)?;
        }
        Ok(view)
    }

    async fn run_quick_spec(
        &self,
        project_id: ProjectId,
        request: &QuickRequest,
        cause: &Cause,
    ) -> Result<QuickView> {
        let input = &request.spec;
        if let Some(problem) = check_quick(input).into_iter().next() {
            return Err(EngineError::InvalidItem {
                field: problem.field,
                code: problem.code.to_string(),
            });
        }
        let (project, repo) = self.workspace_of(project_id)?;
        if request.approve && !project.workflow.spec {
            return Err(Rejection::EntryPointDisabled { entry: "spec" }.into());
        }
        if let Some(name) = &input.repo
            && !project.repos.iter().any(|r| &r.name == name)
        {
            return Err(invalid("repo", "unknown"));
        }
        let path = match &request.path {
            Some(path) => path.clone(),
            None => {
                let slug = quick_slug(input.kind, &input.title)
                    .ok_or_else(|| invalid("title", "no_slug"))?;
                match project.project_path.as_str() {
                    "." => format!("specs/{slug}.md"),
                    folder => format!("{folder}/specs/{slug}.md"),
                }
            }
        };
        paths::check_spec_path(&project.project_path, &path)
            .map_err(|issue| invalid("path", path_code(issue)))?;

        let file_name = path.rsplit('/').next().unwrap_or(&path);
        let slug = file_name
            .strip_suffix(".md")
            .unwrap_or(file_name)
            .to_string();
        let text = render_quick(input);
        let parsed = parse(file_name, text.as_bytes());
        if !parsed
            .model
            .as_ref()
            .is_some_and(|model| quick_keeps_structure(input, model))
        {
            return Err(Rejection::QuickSpecRenderMismatch.into());
        }
        if !parsed.is_ready() {
            let issues = serde_json::to_value(&parsed.issues)
                .map_err(|e| EngineError::Internal(e.to_string()))?;
            return Err(Rejection::SpecNotReady { issues }.into());
        }
        if let Some(other) = self
            .store
            .read(|c| list::spec_by_slug(c, project_id, &slug))?
        {
            return Err(Rejection::SpecSlugConflict {
                slug,
                path: other.path,
            }
            .into());
        }

        let taken = || Rejection::FilePathConflict { path: path.clone() };
        let written = match self
            .commit_doc(
                &project,
                &repo,
                &path,
                text.into_bytes(),
                Precondition::Absent,
                None,
            )
            .await
        {
            Ok(written) => written,
            Err(EngineError::Rejected(Rejection::FileStale { .. })) => return Err(taken().into()),
            Err(other) => return Err(other),
        };
        let Some(event) = &written.event else {
            return Err(taken().into());
        };
        let now = self.now();
        let mut batch = Batch::new(now);
        self.push_event(&mut batch, event, Some(project_id), cause, now);
        self.commit(&batch)?;
        let file = SpecFile {
            path: &path,
            source_commit: &written.commit,
            bytes: &written.bytes,
            origin: request.origin,
        };
        let (registered, _) = self.register_at(&project, &repo, file, cause, None).await?;
        if !request.approve {
            return Ok(QuickView {
                spec: registered,
                path,
                commit_sha: written.commit,
                tasks: Vec::new(),
            });
        }
        let approved = match self.approve_spec_with(
            registered.spec.id,
            registered.revision.id,
            Some(QUICK_REASON.to_string()),
            cause,
            None,
        ) {
            Ok(view) => view,
            Err(EngineError::Rejected(rejection)) => {
                return Err(Rejection::QuickSpecNotApproved {
                    code: rejection.code().to_string(),
                    spec: spec_summary(&registered),
                }
                .into());
            }
            Err(other) => return Err(other),
        };
        let filter = list::WorkItemFilter {
            project: None,
            spec: Some(approved.spec.id),
            states: Vec::new(),
        };
        let page = list::Page {
            after: None,
            limit: TASK_PAGE,
        };
        let tasks = self.work_items(&filter, &page)?.items;
        Ok(QuickView {
            spec: approved,
            path,
            commit_sha: written.commit,
            tasks,
        })
    }
}
