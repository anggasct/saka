use saka_core::deploy::Deployment;
use saka_core::diagnostics::{self as eval, Finding, IncidentSourceFacts, SecretFileState};
use saka_core::ids::{IncidentId, ProjectId, SpecDraftId, SpecId, WorkItemId};
use saka_core::incident::{Incident, Source};
use saka_core::model::Project;
use saka_core::rejection::Rejection;
use saka_core::vocab::{IncidentSeverity, IncidentState, SpecDraftState, SpecState, WorkItemState};
use saka_store::incident::IncidentFilter;
use saka_store::list::{self, Page};
use saka_store::{deployment, incident, issue_intake, read};

use crate::{Engine, EngineError, Paged, Result};

#[derive(Debug, Clone, PartialEq)]
pub struct SourceView {
    pub source: Source,
    pub open: u32,
    pub endpoint_path: String,
    pub endpoint: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct IncidentRow {
    pub incident: Incident,
    pub deployment: Option<Deployment>,
    pub draft: Option<(SpecDraftId, SpecDraftState)>,
    pub spec: Option<(SpecId, String, SpecState)>,
    pub revert: Option<(WorkItemId, String, WorkItemState)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Suspect {
    Task {
        work_item_id: WorkItemId,
        key: String,
        change: Option<(u64, String)>,
    },
    Change {
        number: u64,
        url: String,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct IncidentDetail {
    pub row: IncidentRow,
    pub range: Option<(Option<String>, String)>,
    pub suspects: Vec<Suspect>,
}

#[derive(Debug, Clone, Default)]
pub struct IncidentQuery {
    pub state: Option<IncidentState>,
    pub severity: Option<IncidentSeverity>,
    pub environment: Option<String>,
    pub source: Option<String>,
}

impl Engine {
    fn sources_of(&self, project: &Project) -> Vec<&Source> {
        self.cfg
            .incident_sources
            .iter()
            .filter(|s| s.is_for(project.id, &project.name))
            .collect()
    }

    pub fn incident_sources(&self, project_id: ProjectId) -> Result<Vec<SourceView>> {
        let project = self.load_project_row(project_id)?;
        let live = self.store.read(|c| incident::live_counts(c, project.id))?;
        let mut views: Vec<SourceView> = self
            .sources_of(&project)
            .into_iter()
            .map(|source| {
                let endpoint_path = format!("/hooks/incident/{}", source.name);
                SourceView {
                    endpoint: self
                        .cfg
                        .public_url
                        .as_deref()
                        .map(|base| format!("{}{endpoint_path}", base.trim_end_matches('/'))),
                    endpoint_path,
                    open: live
                        .iter()
                        .find(|(name, _)| *name == source.name)
                        .map_or(0, |(_, count)| *count),
                    source: source.clone(),
                }
            })
            .collect();
        views.sort_by(|a, b| a.source.name.cmp(&b.source.name));
        Ok(views)
    }

    pub fn incidents(
        &self,
        project_id: ProjectId,
        query: IncidentQuery,
        page: &Page,
    ) -> Result<Paged<IncidentRow>> {
        self.load_project_row(project_id)?;
        let filter = IncidentFilter {
            project: Some(project_id),
            state: query.state,
            severity: query.severity,
            environment: query.environment,
            source: query.source,
        };
        let rows = self.store.read(|c| {
            incident::list(
                c,
                &filter,
                &Page {
                    after: page.after,
                    limit: page.limit.saturating_add(1),
                },
            )
        })?;
        let paged = crate::views::paged(rows, page, |(_, key)| *key);
        let mut items = Vec::with_capacity(paged.items.len());
        for (incident, _) in paged.items {
            items.push(self.incident_row(incident)?);
        }
        Ok(Paged {
            items,
            next: paged.next,
        })
    }

    pub(crate) fn incident_row(&self, incident: Incident) -> Result<IncidentRow> {
        let deployment = match incident.deployment_id {
            Some(id) => self.store.read(|c| deployment::load(c, id))?,
            None => None,
        };
        let (draft, spec, revert) = self.store.read(|c| {
            let outcome = match incident.draft_id {
                Some(id) => issue_intake::draft_outcome(c, id)?.map(|o| (id, o)),
                None => None,
            };
            let spec = match outcome.as_ref().and_then(|(_, o)| o.spec_id) {
                Some(id) => read::load_spec(c, id)?.map(|s| (s.id, s.path, s.state)),
                None => None,
            };
            let revert = match incident.revert_id {
                Some(id) => incident::revert_brief(c, id)?.map(|(key, state)| (id, key, state)),
                None => None,
            };
            Ok((outcome.map(|(id, o)| (id, o.state)), spec, revert))
        })?;
        Ok(IncidentRow {
            incident,
            deployment,
            draft,
            spec,
            revert,
        })
    }

    pub fn incident(&self, id: IncidentId) -> Result<IncidentDetail> {
        let found = self
            .store
            .read(|c| incident::load(c, id))?
            .ok_or(EngineError::Rejected(Rejection::NotFound))?;
        let row = self.incident_row(found)?;
        let Some(linked) = &row.deployment else {
            return Ok(IncidentDetail {
                row,
                range: None,
                suspects: Vec::new(),
            });
        };
        let (tasks, merges) = self.store.read(|c| {
            Ok((
                deployment::tasks_of(c, linked.id)?,
                deployment::merge_refs(c, linked.id)?,
            ))
        })?;
        let mut suspects: Vec<Suspect> = tasks
            .iter()
            .map(|task| Suspect::Task {
                work_item_id: task.work_item_id,
                key: task.task_key.clone(),
                change: merges
                    .iter()
                    .find(|m| m.work_item_id == task.work_item_id)
                    .map(|m| (m.number, m.url.clone())),
            })
            .collect();
        suspects.extend(
            merges
                .iter()
                .filter(|m| !tasks.iter().any(|t| t.work_item_id == m.work_item_id))
                .map(|m| Suspect::Change {
                    number: m.number,
                    url: m.url.clone(),
                }),
        );
        let range = linked
            .sha
            .clone()
            .map(|sha| (linked.previous_sha.clone(), sha));
        Ok(IncidentDetail {
            row,
            range,
            suspects,
        })
    }

    pub(crate) fn incident_suspect_count(&self, incident: &Incident) -> usize {
        let Some(deployment) = incident.deployment_id else {
            return 0;
        };
        let found = self.store.read(|c| {
            Ok((
                deployment::tasks_of(c, deployment)?,
                deployment::merge_refs(c, deployment)?,
            ))
        });
        let Ok((tasks, merges)) = found else {
            return 0;
        };
        tasks.len()
            + merges
                .iter()
                .filter(|m| !tasks.iter().any(|t| t.work_item_id == m.work_item_id))
                .count()
    }

    pub(crate) async fn incident_source_findings(&self) -> Vec<Finding> {
        let projects = match self.store.read(|c| {
            list::project_ids(c)?
                .into_iter()
                .map(|id| read::load_project(c, id))
                .collect::<std::result::Result<Vec<_>, _>>()
        }) {
            Ok(found) => found.into_iter().flatten().collect::<Vec<Project>>(),
            Err(e) => return vec![eval::check_failed(e.code())],
        };
        let sources = self.cfg.incident_sources.clone();
        let facts = tokio::task::spawn_blocking(move || {
            sources
                .iter()
                .map(|source| {
                    let project = projects.iter().find(|p| source.is_for(p.id, &p.name));
                    IncidentSourceFacts {
                        name: source.name.clone(),
                        project_known: project.is_some(),
                        binding: project.map_or(Ok(()), |p| source.bind(&p.repos).map(|_| ())),
                        secret: secret_file_state(&source.secret_file),
                    }
                })
                .collect::<Vec<_>>()
        })
        .await;
        match facts {
            Ok(facts) => eval::incident_source(&facts),
            Err(_) => vec![eval::check_failed("incident_facts_failed")],
        }
    }
}

fn secret_file_state(path: &str) -> SecretFileState {
    use std::os::unix::fs::PermissionsExt;
    let Ok(meta) = std::fs::metadata(path) else {
        return SecretFileState::Missing;
    };
    if !meta.is_file() {
        return SecretFileState::Missing;
    }
    if meta.permissions().mode() & 0o777 != 0o600 {
        return SecretFileState::WrongMode;
    }
    match std::fs::read(path) {
        Ok(bytes) if bytes.iter().all(|b| b.is_ascii_whitespace()) => SecretFileState::Empty,
        Ok(_) => SecretFileState::Private,
        Err(_) => SecretFileState::Missing,
    }
}
