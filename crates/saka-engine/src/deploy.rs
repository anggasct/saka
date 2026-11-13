use std::collections::BTreeMap;

use saka_core::Ctx;
use saka_core::deploy::{self, CheckRecord, Deployment, Input, QueueRequest};
use saka_core::diagnostics::{
    self as eval, DeployEnvironmentFacts, DeployFacts, Finding, VerifyFacts,
};
use saka_core::environment::{DeployAction, Environment, VerifySpec};
use saka_core::event::Event;
use saka_core::ids::{ChangeRequestId, SpecDraftId, WorkItemId};
use saka_core::ids::{DeploymentId, ProjectId, RepoId};
use saka_core::model::Task;
use saka_core::model::{Project, Repo};
use saka_core::rejection::Rejection;
use saka_core::task::{self, TaskInput};
use saka_core::time::Timestamp;
use saka_core::vocab::{
    BlockerCode, ChangeRole, DeployCause, DeployGateState, DeploymentState, Stage, StagePhase,
    Trigger, WorkItemOrigin,
};
use saka_core::vocab::{CheckId, DeployActionKind, OnFailure};
use saka_ports::IdsSource;
use saka_store::Batch;
use saka_store::deployment::DeployGateRow;
use saka_store::deployment::{DeploymentFilter, MergeRef, TaskRef};
use saka_store::list::Page;
use saka_store::{deployment, read};

use crate::action::{Action, Produced};
use crate::commands::reason;
use crate::draft::NewSpecDraft;
use crate::views::Paged;
use crate::{Cause, Engine, EngineError, Extra, Mode, Result, Shows};
use saka_core::deploy::{FixFacts, FixFailure};
use saka_core::ids::BlockerId;
use saka_core::model::Draft;
use saka_core::vocab::{
    BlockerAction, DeployCheckState, DraftLocale, DraftMode, SpecDraftState, TargetKind, WorkKind,
};
use saka_store::draft as draft_rows;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoInfo {
    pub id: RepoId,
    pub name: String,
    pub full_name: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VerificationView {
    pub ready: bool,
    pub commands: usize,
    pub warnings: Vec<&'static str>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EnvironmentView {
    pub name: String,
    pub repo: RepoInfo,
    pub branch: String,
    pub action: DeployActionKind,
    pub target_url: Option<String>,
    pub gate: bool,
    pub on_failure: OnFailure,
    pub verification: VerificationView,
    pub queued: Option<Deployment>,
    pub active: Option<Deployment>,
    pub last_succeeded: Option<Deployment>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DeploymentRow {
    pub deployment: Deployment,
    pub repo: RepoInfo,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DeploymentDetail {
    pub row: DeploymentRow,
    pub checks: Vec<CheckRecord>,
    pub merges: Vec<MergeRef>,
    pub tasks: Vec<TaskRef>,
    pub fix_draft: Option<(SpecDraftId, SpecDraftState)>,
}

pub(crate) fn verification_view(
    action: &DeployAction,
    verify: Option<&VerifySpec>,
) -> VerificationView {
    let mut warnings = Vec::new();
    if verify.is_none() {
        warnings.push("verify_missing");
    }
    let ready = verify.is_some_and(|v| v.ready.is_some());
    if matches!(action, DeployAction::Webhook { .. }) && !ready {
        warnings.push("ready_missing");
    }
    VerificationView {
        ready,
        commands: verify.map_or(0, |v| v.commands.len()),
        warnings,
    }
}

fn repo_info(project: &Project, repo: &Repo) -> RepoInfo {
    RepoInfo {
        id: repo.id,
        name: project
            .repos
            .iter()
            .find(|b| b.repo_id == repo.id)
            .map_or_else(|| repo.full_name.clone(), |b| b.name.clone()),
        full_name: repo.full_name.clone(),
    }
}

impl Engine {
    pub(crate) fn environments_of(&self, project: &Project) -> Vec<&Environment> {
        self.cfg
            .environments
            .iter()
            .filter(|e| e.is_for(project.id, &project.name))
            .collect()
    }

    fn load_project_row(&self, id: ProjectId) -> Result<Project> {
        self.store
            .read(|c| read::load_project(c, id))?
            .ok_or(EngineError::Rejected(Rejection::NotFound))
    }

    fn repo_infos(&self, project: &Project) -> Result<BTreeMap<RepoId, RepoInfo>> {
        let repos = self.store.read(|c| {
            project
                .repos
                .iter()
                .map(|b| read::load_repo(c, b.repo_id))
                .collect::<std::result::Result<Vec<_>, _>>()
        })?;
        Ok(repos
            .into_iter()
            .flatten()
            .map(|r| (r.id, repo_info(project, &r)))
            .collect())
    }

    pub fn environments(&self, project_id: ProjectId) -> Result<Vec<EnvironmentView>> {
        let project = self.load_project_row(project_id)?;
        let infos = self.repo_infos(&project)?;
        let mut views = Vec::new();
        for env in self.environments_of(&project) {
            let Ok(bound) = env.bind(&project.repos) else {
                continue;
            };
            let Some(repo) = infos.get(&bound.repo.repo_id) else {
                continue;
            };
            let (queued, active, last_succeeded) = self.store.read(|c| {
                Ok((
                    deployment::queued_of(c, project.id, &env.name)?,
                    deployment::active_of(c, project.id, &env.name)?,
                    deployment::last_succeeded(c, project.id, &env.name)?,
                ))
            })?;
            views.push(EnvironmentView {
                name: env.name.clone(),
                repo: repo.clone(),
                branch: bound.branch,
                action: env.action.kind(),
                target_url: env.target_url.clone(),
                gate: env.gate,
                on_failure: env.on_failure,
                verification: verification_view(&env.action, project.verify.get(&env.name)),
                queued,
                active,
                last_succeeded,
            });
        }
        views.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(views)
    }

    pub fn deployments(
        &self,
        project_id: ProjectId,
        environment: Option<String>,
        state: Option<saka_core::vocab::DeploymentState>,
        page: &Page,
    ) -> Result<Paged<DeploymentRow>> {
        let project = self.load_project_row(project_id)?;
        let infos = self.repo_infos(&project)?;
        let filter = DeploymentFilter {
            project: Some(project_id),
            environment,
            state,
        };
        let rows = self.store.read(|c| {
            deployment::list(
                c,
                &filter,
                &Page {
                    after: page.after,
                    limit: page.limit.saturating_add(1),
                },
            )
        })?;
        let paged = crate::views::paged(rows, page, |(_, key)| *key);
        let mut items = Vec::new();
        for (deployment, _) in paged.items {
            let repo = infos
                .get(&deployment.repo_id)
                .cloned()
                .ok_or_else(|| EngineError::Internal("deployment without its repo".into()))?;
            items.push(DeploymentRow { deployment, repo });
        }
        Ok(Paged {
            items,
            next: paged.next,
        })
    }

    pub fn deployment(&self, id: DeploymentId) -> Result<DeploymentDetail> {
        let found = self
            .store
            .read(|c| deployment::load(c, id))?
            .ok_or(EngineError::Rejected(Rejection::NotFound))?;
        let project = self.load_project_row(found.project_id)?;
        let repo = self
            .repo_infos(&project)?
            .remove(&found.repo_id)
            .ok_or_else(|| EngineError::Internal("deployment without its repo".into()))?;
        let (checks, merges, tasks, fix_draft) = self.store.read(|c| {
            Ok((
                deployment::checks(c, id)?,
                deployment::merge_refs(c, id)?,
                deployment::tasks_of(c, id)?,
                draft_rows::fix_draft_of(c, id)?,
            ))
        })?;
        Ok(DeploymentDetail {
            row: DeploymentRow {
                deployment: found,
                repo,
            },
            checks,
            merges,
            tasks,
            fix_draft,
        })
    }
}

fn secret_names(env: &Environment) -> Vec<String> {
    let own: Vec<String> = match &env.action {
        DeployAction::Run { secrets, .. } => secrets.clone(),
        DeployAction::Webhook { secret, .. } => vec![secret.clone()],
        DeployAction::RunHost { .. } => Vec::new(),
    };
    let mut names: Vec<String> = own.into_iter().chain(env.verify_secrets.clone()).collect();
    names.sort();
    names.dedup();
    names
}

impl Engine {
    async fn deploy_facts(&self, project: &Project, remote: bool) -> Result<DeployFacts> {
        let stored: Vec<String> = if remote {
            self.store
                .read(|c| read::project_secrets(c, project.id))?
                .into_iter()
                .map(|s| s.name)
                .collect()
        } else {
            Vec::new()
        };
        let mut environments = Vec::new();
        for env in self.environments_of(project) {
            let bound = env.bind(&project.repos);
            let branch_found = match (&bound, remote) {
                (Ok(b), true) => self.deploy_branch_found(b.repo.repo_id, &b.branch).await,
                _ => None,
            };
            environments.push(DeployEnvironmentFacts {
                name: env.name.clone(),
                action: env.action.kind(),
                binding: bound.map(|b| (b.repo.name.clone(), b.branch)),
                branch_found,
                missing_secrets: if remote {
                    secret_names(env)
                        .into_iter()
                        .filter(|n| !stored.contains(n))
                        .collect()
                } else {
                    Vec::new()
                },
                verify: project.verify.get(&env.name).map(|v| VerifyFacts {
                    has_ready: v.ready.is_some(),
                }),
            });
        }
        let unknown_verify = project
            .verify
            .keys()
            .filter(|name| !environments.iter().any(|e| &e.name == *name))
            .cloned()
            .collect();
        Ok(DeployFacts {
            environments,
            unknown_verify,
        })
    }

    async fn deploy_branch_found(&self, repo_id: RepoId, branch: &str) -> Option<bool> {
        let repo = self.repo_ref_of(repo_id).ok()?;
        match self.ports.env.branch_tip(&repo, branch).await {
            Ok(tip) => Some(tip.is_some()),
            Err(_) => None,
        }
    }

    pub(crate) async fn deploy_findings(&self, project: &Project, id: CheckId) -> Vec<Finding> {
        let remote = id == CheckId::DeployConfig;
        let facts = match self.deploy_facts(project, remote).await {
            Ok(facts) => facts,
            Err(EngineError::Store(e)) => return vec![eval::check_failed(e.code())],
            Err(_) => return vec![eval::check_failed("deploy_facts_failed")],
        };
        match id {
            CheckId::DeployConfig => eval::deploy_config(&facts),
            CheckId::DeployVerifyMissing => eval::deploy_verify_missing(&facts),
            _ => eval::deploy_ready_missing(&facts),
        }
    }
}

fn batch_queued<'a>(
    batch: &'a Batch,
    project: ProjectId,
    environment: &str,
) -> Option<&'a Deployment> {
    batch.deployments.iter().find(|d| {
        d.project_id == project
            && d.environment == environment
            && d.state == DeploymentState::Queued
    })
}

fn gate_row(event: &Event) -> Option<DeployGateRow> {
    let Event::DeployGateChanged {
        work_item_id,
        environment,
        state,
        deployment_id,
        reason,
    } = event
    else {
        return None;
    };
    Some(DeployGateRow {
        work_item_id: *work_item_id,
        environment: environment.clone(),
        state: *state,
        deployment_id: *deployment_id,
        reason: reason.clone(),
    })
}

impl Engine {
    pub(crate) fn plan_deployments(
        &self,
        batch: &mut Batch,
        task: &mut Task,
        event: &Event,
        cause: &Cause,
        now: Timestamp,
    ) -> Result<()> {
        match event {
            Event::DeployGateChanged { .. } => {
                batch.deploy_gates.extend(gate_row(event));
            }
            Event::ChangeRequestMerged {
                role: ChangeRole::Code,
                change_request_id,
                ..
            } if !self.cfg.environments.is_empty() => {
                self.queue_merge_deployments(batch, task, *change_request_id, cause, now)?;
            }
            Event::StageRunCreated {
                stage: Stage::Sync,
                entry_phase: StagePhase::TearingDown,
                retry_of: Some(retried),
                trigger: Trigger::Manual,
                ..
            } if retried.code == BlockerCode::DeployUnverified.as_str() => {
                self.retry_deploy_gates(batch, task, cause, now)?;
            }
            _ => {}
        }
        Ok(())
    }

    fn emit_gate(
        &self,
        batch: &mut Batch,
        task: &mut Task,
        event: Event,
        cause: &Cause,
        now: Timestamp,
    ) {
        if let Some(next) = task::evolve(Some(task.clone()), &event, now) {
            *task = next;
        }
        self.push_event(batch, &event, Some(task.item.project_id), cause, now);
        batch.deploy_gates.extend(gate_row(&event));
    }

    fn put_deployment(&self, batch: &mut Batch, deployment: Deployment) {
        match batch.deployments.iter_mut().find(|d| d.id == deployment.id) {
            Some(slot) => *slot = deployment,
            None => batch.deployments.push(deployment),
        }
    }

    fn queued_for(
        &self,
        batch: &Batch,
        project: ProjectId,
        environment: &str,
    ) -> Result<Option<Deployment>> {
        match batch_queued(batch, project, environment) {
            Some(found) => Ok(Some(found.clone())),
            None => Ok(self
                .store
                .read(|c| deployment::queued_of(c, project, environment))?),
        }
    }

    pub(crate) fn queue_deployment(
        &self,
        batch: &mut Batch,
        project: &Project,
        env: &Environment,
        request: QueueRequest,
        cause: &Cause,
        now: Timestamp,
    ) -> Result<(Deployment, bool)> {
        let existing = self.queued_for(batch, project.id, &env.name)?;
        let mut ids = IdsSource(&*self.ports.ids);
        let events = deploy::queue(existing.as_ref(), request, &mut Ctx { now, ids: &mut ids })?;
        let created = existing.is_none();
        for event in &events {
            self.push_event(batch, event, Some(project.id), cause, now);
        }
        let target = deploy::evolve_all(existing, &events, now).ok_or_else(|| {
            EngineError::Internal("a deployment request produced no deployment".into())
        })?;
        self.put_deployment(batch, target.clone());
        Ok((target, created))
    }

    fn queue_merge_deployments(
        &self,
        batch: &mut Batch,
        task: &mut Task,
        change_request_id: ChangeRequestId,
        cause: &Cause,
        now: Timestamp,
    ) -> Result<()> {
        let Some(base_branch) = task.change_request.as_ref().map(|c| c.base_branch.clone()) else {
            return Ok(());
        };
        let Some(project) = self
            .store
            .read(|c| read::load_project(c, task.item.project_id))?
        else {
            return Ok(());
        };
        let gated_origin = task.item.origin != WorkItemOrigin::ExternalChange;
        let environments: Vec<Environment> = self
            .environments_of(&project)
            .into_iter()
            .cloned()
            .collect();
        for env in environments {
            let Ok(bound) = env.bind(&project.repos) else {
                continue;
            };
            if bound.repo.repo_id != task.item.repo_id || bound.branch != base_branch {
                continue;
            }
            let request = QueueRequest {
                project_id: project.id,
                repo_id: task.item.repo_id,
                environment: env.name.clone(),
                branch: bound.branch.clone(),
                cause: DeployCause::Merge,
                action: env.action.kind(),
                target_url: env.target_url.clone(),
                on_failure: env.on_failure,
                actor: None,
                reason: None,
                change_request_id: Some(change_request_id),
            };
            let (target, created) =
                self.queue_deployment(batch, &project, &env, request, cause, now)?;
            if created {
                let mut excluded = vec![task.item.id];
                excluded.extend(task.revert.as_ref().map(|r| r.of));
                self.attach_open_gates(
                    batch, &project, &env.name, target.id, &excluded, cause, now,
                )?;
            }
            if env.gate && gated_origin {
                let event = Event::DeployGateChanged {
                    work_item_id: task.item.id,
                    environment: env.name.clone(),
                    state: DeployGateState::Pending,
                    deployment_id: Some(target.id),
                    reason: None,
                };
                self.emit_gate(batch, task, event, cause, now);
            }
        }
        Ok(())
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "one step of attaching gates, with the batch and the cause passed through"
    )]
    pub(crate) fn attach_open_gates(
        &self,
        batch: &mut Batch,
        project: &Project,
        environment: &str,
        target: DeploymentId,
        except: &[WorkItemId],
        cause: &Cause,
        now: Timestamp,
    ) -> Result<()> {
        let gates = self
            .store
            .read(|c| deployment::open_gates_of(c, project.id, environment))?;
        for gate in gates {
            if except.contains(&gate.work_item_id) || gate.deployment_id == Some(target) {
                continue;
            }
            let current = match gate.deployment_id {
                Some(id) => self.store.read(|c| deployment::load(c, id))?,
                None => None,
            };
            let attachable = current.is_none_or(|d| {
                matches!(
                    d.state,
                    DeploymentState::Failed | DeploymentState::Cancelled
                )
            });
            if attachable {
                self.repoint_gate(
                    batch,
                    &gate,
                    gate.state,
                    Some(target),
                    gate.reason.clone(),
                    cause,
                    now,
                )?;
            }
        }
        Ok(())
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "one gate change with the batch and the cause passed through"
    )]
    fn repoint_gate(
        &self,
        batch: &mut Batch,
        gate: &DeployGateRow,
        state: DeployGateState,
        deployment_id: Option<DeploymentId>,
        reason: Option<String>,
        cause: &Cause,
        now: Timestamp,
    ) -> Result<()> {
        let existing = batch
            .tasks
            .iter()
            .position(|t| t.item.id == gate.work_item_id);
        let loaded = match existing {
            Some(at) => Some(batch.tasks.remove(at)),
            None => self
                .store
                .read(|c| read::load_task(c, gate.work_item_id, &self.cfg.workflow_policy))?,
        };
        let Some(mut other) = loaded else {
            return Ok(());
        };
        let event = Event::DeployGateChanged {
            work_item_id: gate.work_item_id,
            environment: gate.environment.clone(),
            state,
            deployment_id,
            reason,
        };
        self.emit_gate(batch, &mut other, event, cause, now);
        batch.tasks.push(other);
        Ok(())
    }

    pub(crate) fn skip_gate(
        &self,
        batch: &mut Batch,
        gate: &DeployGateRow,
        cause: &Cause,
    ) -> Result<()> {
        let now = batch.now;
        self.repoint_gate(
            batch,
            gate,
            DeployGateState::Skipped,
            None,
            Some("environment_removed".to_string()),
            cause,
            now,
        )
    }

    fn retry_deploy_gates(
        &self,
        batch: &mut Batch,
        task: &mut Task,
        cause: &Cause,
        now: Timestamp,
    ) -> Result<()> {
        let Some(project) = self
            .store
            .read(|c| read::load_project(c, task.item.project_id))?
        else {
            return Ok(());
        };
        let failed: Vec<String> = task
            .deploy_gates
            .iter()
            .filter(|g| g.state == DeployGateState::Failed)
            .map(|g| g.environment.clone())
            .collect();
        for name in failed {
            let found = self
                .environments_of(&project)
                .into_iter()
                .find(|e| e.name == name)
                .cloned();
            let bound = found.as_ref().and_then(|e| e.bind(&project.repos).ok());
            let (Some(env), Some(bound)) = (found, bound) else {
                let event = Event::DeployGateChanged {
                    work_item_id: task.item.id,
                    environment: name,
                    state: DeployGateState::Skipped,
                    deployment_id: None,
                    reason: Some("environment_removed".to_string()),
                };
                self.emit_gate(batch, task, event, cause, now);
                continue;
            };
            let request = QueueRequest {
                project_id: project.id,
                repo_id: bound.repo.repo_id,
                environment: env.name.clone(),
                branch: bound.branch.clone(),
                cause: DeployCause::Retry,
                action: env.action.kind(),
                target_url: env.target_url.clone(),
                on_failure: env.on_failure,
                actor: Some(cause.actor.clone()),
                reason: None,
                change_request_id: None,
            };
            let existing = self.queued_for(batch, project.id, &env.name)?;
            let target = match existing {
                Some(queued) => queued,
                None => {
                    let (created, _) =
                        self.queue_deployment(batch, &project, &env, request, cause, now)?;
                    created
                }
            };
            self.attach_open_gates(
                batch,
                &project,
                &env.name,
                target.id,
                &[task.item.id],
                cause,
                now,
            )?;
            let event = Event::DeployGateChanged {
                work_item_id: task.item.id,
                environment: env.name.clone(),
                state: DeployGateState::Pending,
                deployment_id: Some(target.id),
                reason: None,
            };
            self.emit_gate(batch, task, event, cause, now);
        }
        Ok(())
    }

    pub fn deploy_environment_with(
        &self,
        project_id: ProjectId,
        environment: &str,
        why: &str,
        cause: &Cause,
        action: Option<&Action<'_>>,
    ) -> Result<DeploymentRow> {
        let why = reason(why)?;
        let _guard = self.locked()?;
        let now = self.now();
        let project = self.load_project_row(project_id)?;
        let env = self
            .environments_of(&project)
            .into_iter()
            .find(|e| e.name == environment)
            .cloned()
            .ok_or(EngineError::Rejected(Rejection::EnvironmentNotFound))?;
        let bound = env
            .bind(&project.repos)
            .map_err(|_| EngineError::Rejected(Rejection::EnvironmentNotFound))?;
        let mut batch = Batch::new(now);
        let request = QueueRequest {
            project_id,
            repo_id: bound.repo.repo_id,
            environment: env.name.clone(),
            branch: bound.branch.clone(),
            cause: DeployCause::Redeploy,
            action: env.action.kind(),
            target_url: env.target_url.clone(),
            on_failure: env.on_failure,
            actor: Some(cause.actor.clone()),
            reason: Some(why),
            change_request_id: None,
        };
        let (created, _) =
            self.queue_deployment(&mut batch, &project, &env, request, cause, now)?;
        self.attach_open_gates(&mut batch, &project, &env.name, created.id, &[], cause, now)?;
        let infos = self.repo_infos(&project)?;
        let row = DeploymentRow {
            repo: infos
                .get(&created.repo_id)
                .cloned()
                .ok_or_else(|| EngineError::Internal("deployment without its repo".into()))?,
            deployment: created,
        };
        if let Some(a) = action {
            let reply = (a.respond)(Produced::Deployment(&row));
            batch.action = Some(a.record(
                self,
                cause,
                Some(project_id),
                Some(row.deployment.id.0),
                reply,
            ));
        }
        self.commit(&batch)?;
        Ok(row)
    }

    pub fn cancel_deployment_with(
        &self,
        id: DeploymentId,
        why: &str,
        cause: &Cause,
        action: Option<&Action<'_>>,
    ) -> Result<DeploymentRow> {
        let why = reason(why)?;
        let _guard = self.locked()?;
        let now = self.now();
        let current = self
            .store
            .read(|c| deployment::load(c, id))?
            .ok_or(EngineError::Rejected(Rejection::NotFound))?;
        let events = deploy::decide(
            &current,
            Input::Cancel {
                reason: why,
                actor: Some(cause.actor.clone()),
            },
        )?;
        let cancelled = deploy::evolve_all(Some(current.clone()), &events, now)
            .ok_or_else(|| EngineError::Internal("a cancelled deployment vanished".into()))?;
        let mut batch = Batch::new(now);
        for event in &events {
            self.push_event(&mut batch, event, Some(current.project_id), cause, now);
        }
        let queued = self.queued_for(&batch, current.project_id, &current.environment)?;
        let gates = self.store.read(|c| deployment::gates_of(c, id))?;
        if let Some(target) = queued.filter(|q| q.id != id) {
            for gate in gates
                .iter()
                .filter(|g| matches!(g.state, DeployGateState::Pending | DeployGateState::Failed))
            {
                self.repoint_gate(
                    &mut batch,
                    gate,
                    gate.state,
                    Some(target.id),
                    gate.reason.clone(),
                    cause,
                    now,
                )?;
            }
        }
        batch.deployments.push(cancelled.clone());
        let project = self.load_project_row(current.project_id)?;
        let infos = self.repo_infos(&project)?;
        let row = DeploymentRow {
            repo: infos
                .get(&cancelled.repo_id)
                .cloned()
                .ok_or_else(|| EngineError::Internal("deployment without its repo".into()))?,
            deployment: cancelled,
        };
        if let Some(a) = action {
            let reply = (a.respond)(Produced::Deployment(&row));
            batch.action = Some(a.record(self, cause, Some(current.project_id), Some(id.0), reply));
        }
        self.counters.deployment(&current.environment, "cancelled");
        self.commit(&batch)?;
        Ok(row)
    }

    pub fn accept_unverified_with(
        &self,
        id: WorkItemId,
        blocker_id: saka_core::ids::BlockerId,
        why: &str,
        cause: &Cause,
        action: Option<&Action<'_>>,
    ) -> Result<Vec<Event>> {
        let reason = reason(why)?;
        self.decide(
            id,
            Some(TaskInput::AcceptUnverified { blocker_id, reason }),
            Mode::Command,
            cause,
            Extra {
                action: action.map(|a| (a, Shows::Task)),
                ..Extra::default()
            },
        )
    }
}

impl Engine {
    pub async fn draft_fix_with(
        &self,
        id: DeploymentId,
        note: Option<&str>,
        cause: &Cause,
        action: Option<&Action<'_>>,
    ) -> Result<Draft> {
        if note.is_some_and(|n| n.chars().count() > NOTE_MAX) {
            return Err(crate::register::invalid("note", "too_long"));
        }
        let detail = self.deployment(id)?;
        let deployment = &detail.row.deployment;
        if deployment.state != DeploymentState::Failed {
            return Err(Rejection::DeploymentNotFailed.into());
        }
        if let Some((spec_draft_id, state)) =
            self.store.read(|c| draft_rows::fix_draft_of(c, id))?
            && !matches!(state, SpecDraftState::Succeeded | SpecDraftState::Cancelled)
        {
            return Err(Rejection::FixDraftInProgress {
                spec_draft_id: spec_draft_id.0.to_string(),
            }
            .into());
        }
        let project = self.load_project_row(deployment.project_id)?;
        let tasks: Vec<String> = detail.tasks.iter().map(|t| t.task_key.clone()).collect();
        let failures: Vec<FixFailure> = detail
            .checks
            .iter()
            .filter(|c| c.state != DeployCheckState::Passed)
            .map(|c| FixFailure {
                phase: c.phase,
                command: c.command.clone(),
                state: c.state,
                exit_code: c.exit_code,
                output_tail: c.output_tail.clone(),
            })
            .collect();
        let code = deployment
            .error
            .as_ref()
            .map_or("unknown", |e| e.code.as_str());
        let idea = deploy::fix_idea(
            &FixFacts {
                environment: &deployment.environment,
                sha: deployment.sha.as_deref(),
                previous_sha: deployment.previous_sha.as_deref(),
                phase: deployment.phase,
                code,
                tasks: &tasks,
                failures: &failures,
            },
            note,
        );
        let short: String = deployment
            .sha
            .as_deref()
            .unwrap_or(&deployment.id.to_string())
            .chars()
            .take(7)
            .collect();
        let slug = format!("bugfix-deploy-{}-{short}", deployment.environment);
        let path = match project.project_path.as_str() {
            "." => format!("specs/{slug}.md"),
            folder => format!("{folder}/specs/{slug}.md"),
        };
        let draft = self
            .create_spec_draft(
                project.id,
                NewSpecDraft {
                    path,
                    idea,
                    locale: DraftLocale::En,
                    mode: DraftMode::Draft,
                    target_kind: TargetKind::New,
                    kind: WorkKind::Bugfix,
                    context_paths: Vec::new(),
                    deployment_id: Some(id),
                },
                cause,
                action,
            )
            .await?;
        Ok(draft)
    }

    pub async fn primary_action_with(
        &self,
        id: WorkItemId,
        blocker_id: BlockerId,
        expected: Option<BlockerAction>,
        why: &str,
        cause: &Cause,
        action: Option<&Action<'_>>,
    ) -> Result<Vec<Event>> {
        let task = self.load_task(id)?;
        let blocker = task.open_blocker().filter(|b| b.id == blocker_id).cloned();
        let kind = saka_core::blocker::primary_action(&task).map(|p| p.kind);
        let (Some(blocker), Some(kind @ (BlockerAction::Revert | BlockerAction::DraftFix))) =
            (blocker, kind)
        else {
            return self.resolve_blocker_with(id, blocker_id, expected, why, cause, action);
        };
        if let Some(shown) = expected.filter(|e| *e != kind) {
            return Err(Rejection::BlockerActionChanged {
                blocker_id: blocker_id.0.to_string(),
                expected: shown.as_str(),
                actual: kind.as_str(),
            }
            .into());
        }
        if kind == BlockerAction::Revert {
            return self.revert_with(id, task.item.version, why, cause, action);
        }
        let deployment: DeploymentId = blocker
            .params
            .get("deployment_id")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .ok_or(EngineError::Rejected(Rejection::NotFound))?;
        let note = reason(why)?;
        self.draft_fix_with(deployment, Some(&note), cause, None)
            .await?;
        self.decide(
            id,
            None,
            Mode::Command,
            cause,
            Extra {
                action: action.map(|a| (a, Shows::Task)),
                ..Extra::default()
            },
        )
    }
}

const NOTE_MAX: usize = 1000;
