use saka_core::ids::{IssueIntakeId, ProjectId, RepoId, SpecDraftId, SpecId};
use saka_core::issue_intake::IssueIntake;
use saka_core::model::Project;
use saka_core::rejection::Rejection;
use saka_core::vocab::{SpecDraftState, SpecState};
use saka_store::issue_intake as rows;
use saka_store::list::Page;
use saka_store::read;

use crate::views::{Paged, paged, probe};
use crate::{Engine, EngineError, Result};

#[derive(Debug, Clone, PartialEq)]
pub struct IssueRepoView {
    pub id: RepoId,
    pub name: String,
    pub full_name: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct IssueSpecView {
    pub id: SpecId,
    pub path: String,
    pub state: SpecState,
}

#[derive(Debug, Clone, PartialEq)]
pub struct IssueDraftView {
    pub id: SpecDraftId,
    pub state: SpecDraftState,
}

#[derive(Debug, Clone, PartialEq)]
pub struct IssueIntakeView {
    pub intake: IssueIntake,
    pub repo: Option<IssueRepoView>,
    pub spec: Option<IssueSpecView>,
    pub draft: Option<IssueDraftView>,
}

fn view_of(project: &Project, intake: IssueIntake, links: rows::IntakeLinks) -> IssueIntakeView {
    let repo = links.repo.map(|repo| IssueRepoView {
        id: repo.id,
        name: project
            .repos
            .iter()
            .find(|r| r.repo_id == repo.id)
            .map_or_else(|| repo.full_name.clone(), |r| r.name.clone()),
        full_name: repo.full_name,
    });
    let spec = links.spec.map(|s| IssueSpecView {
        id: s.id,
        path: s.path,
        state: s.state,
    });
    let draft = intake
        .draft_id
        .zip(links.draft)
        .map(|(id, state)| IssueDraftView { id, state });
    IssueIntakeView {
        intake,
        repo,
        spec,
        draft,
    }
}

impl Engine {
    pub fn issue_intakes(
        &self,
        project: ProjectId,
        states: &[String],
        page: &Page,
    ) -> Result<Paged<IssueIntakeView>> {
        let (project, found) = self.store.read(|c| {
            let Some(project) = read::load_project(c, project)? else {
                return Ok((None, Vec::new()));
            };
            let found = rows::issue_intakes(c, project.id, states, &probe(page))?;
            Ok((Some(project), found))
        })?;
        let project = project.ok_or(Rejection::NotFound)?;
        let found = paged(found, page, |(_, key)| *key);
        let next = found.next;
        let items = self.store.read(|c| {
            found
                .items
                .into_iter()
                .map(|(intake, _)| {
                    let links = rows::intake_links(c, &intake)?;
                    Ok(view_of(&project, intake, links))
                })
                .collect::<saka_store::Result<Vec<_>>>()
        })?;
        Ok(Paged { items, next })
    }

    pub fn issue_intake(&self, id: IssueIntakeId) -> Result<IssueIntakeView> {
        self.store
            .read(|c| {
                let Some(intake) = rows::load_issue_intake(c, id)? else {
                    return Ok(None);
                };
                let Some(project) = read::load_project(c, intake.project_id)? else {
                    return Ok(None);
                };
                let links = rows::intake_links(c, &intake)?;
                Ok(Some(view_of(&project, intake, links)))
            })?
            .ok_or(EngineError::Rejected(Rejection::NotFound))
    }
}
