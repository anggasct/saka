use saka_core::authorship::CredentialBlock;
use saka_core::ids::ChangeRequestId;
use saka_core::model::Task;
use saka_core::task::TaskInput;
use saka_core::vocab::{
    ChangeCredentialKind, ChangeRole, ChangeState, ScopeStage, Stage, StagePhase,
    TokenInvalidCause, UserTokenCause, WorkItemOrigin,
};
use saka_ports::{
    ChangeCredential, ChangeOpening, MergeOutcome, MergeRequest, OpenChangeRequest, PortError,
    PushOutcome, PushRequest,
};
use serde_json::json;

use super::{DOCS_TITLE_MAX, Effect, Execution, OpenedBy, internal};
use crate::{Engine, summary, text};

impl Engine {
    pub(super) async fn push_docs(&self, task: &Task, local_sha: String) -> Execution {
        let docs = task.environment.as_ref().and_then(|e| e.docs.as_ref());
        let (true, Some(docs)) = (task.is_writing_at(StagePhase::Pushing), docs) else {
            return Ok(Effect::Skipped);
        };
        if saka_core::task::docs_pending(task).as_deref() != Some(local_sha.as_str()) {
            return Ok(Effect::Skipped);
        }
        let outcome = self
            .ports
            .scm
            .push_branch(&PushRequest {
                repo: self.repo_ref_of(docs.repo_id)?,
                branch: docs.branch.clone().unwrap_or_default(),
                local_sha: local_sha.clone(),
                expected_remote_sha: docs.pushed_sha.clone(),
            })
            .await?;
        Ok(Effect::Fact(match outcome {
            PushOutcome::Pushed { sha } => TaskInput::BranchPushed {
                target: ChangeRole::Docs,
                sha,
            },
            PushOutcome::Rejected { remote_sha } => TaskInput::PushRejected {
                target: ChangeRole::Docs,
                remote_sha,
                local_sha,
            },
        }))
    }

    // Read before merging: after a crash the docs change may already be merged or closed.
    pub(super) async fn merge_docs(&self, task: &Task) -> Execution {
        let Some(change) = task
            .docs_change
            .as_ref()
            .filter(|_| task.is_at(Stage::Sync, StagePhase::MergingDocs))
        else {
            return Ok(Effect::Skipped);
        };
        let docs = task.environment.as_ref().and_then(|e| e.docs.as_ref());
        let repo = self.repo_ref_of(change.repo_id)?;
        let snapshot = self.ports.scm.change_request(&repo, change.number).await?;
        let observed = |snapshot: saka_ports::ChangeSnapshot| {
            Ok(Effect::Fact(TaskInput::ChangeObserved {
                role: ChangeRole::Docs,
                state: snapshot.state,
                head_sha: snapshot.head_sha,
                merge_commit_sha: snapshot.merge_commit_sha,
                merged_at: snapshot.merged_at,
                checks: Vec::new(),
            }))
        };
        match snapshot.state {
            ChangeState::Merged | ChangeState::Closed => return observed(snapshot),
            ChangeState::Open if snapshot.head_sha != change.head_sha => return observed(snapshot),
            ChangeState::Open => {}
        }
        let head_sha = change.head_sha.clone();
        let base = format!("refs/remotes/origin/{}", change.base_branch);
        let Some(docs) = docs else {
            return Err(PortError::permanent("docs_checkout_missing", json!({})));
        };
        if let Some(violation) = self
            .docs_violation(task, docs, &base, &head_sha, true, ScopeStage::Sync)
            .await?
        {
            return Ok(Effect::Fact(TaskInput::PathScopeViolated {
                role: ChangeRole::Docs,
                head_sha,
                violation,
            }));
        }
        let title: String = format!(
            "docs: {} (#{})",
            text::change_title(&task.item.work),
            change.number
        );
        let outcome = self
            .ports
            .scm
            .merge(&MergeRequest {
                repo,
                number: change.number,
                head_sha,
                method: self.cfg.loop_cfg.merge_method,
                commit_title: title,
            })
            .await?;
        Ok(Effect::Fact(match outcome {
            MergeOutcome::Merged {
                merge_commit_sha,
                merged_at,
            } => TaskInput::MergeSucceeded {
                role: ChangeRole::Docs,
                merge_commit_sha,
                merged_at,
            },
            MergeOutcome::Rejected { reason } => {
                let status = saka_core::task::merge_status(&reason);
                tracing::warn!(
                    work_item_id = %task.item.id,
                    number = change.number,
                    status,
                    "delivery.docs_change.merge_failed"
                );
                TaskInput::MergeRejected {
                    role: ChangeRole::Docs,
                    reason,
                }
            }
        }))
    }

    pub(super) async fn push(&self, task: &Task, local_sha: String) -> Execution {
        if !task.is_writing_at(StagePhase::Pushing) {
            return Ok(Effect::Skipped);
        }
        let outcome = self
            .ports
            .scm
            .push_branch(&PushRequest {
                repo: self.repo_ref(task)?,
                branch: task.item.branch.clone(),
                local_sha: local_sha.clone(),
                expected_remote_sha: task.change_request.as_ref().map(|c| c.head_sha.clone()),
            })
            .await?;
        if matches!(outcome, PushOutcome::Pushed { .. }) {
            self.refresh_summary(task).await;
        }
        Ok(Effect::Fact(match outcome {
            PushOutcome::Pushed { sha } => TaskInput::BranchPushed {
                target: saka_core::vocab::ChangeRole::Code,
                sha,
            },
            PushOutcome::Rejected { remote_sha } => TaskInput::PushRejected {
                target: saka_core::vocab::ChangeRole::Code,
                remote_sha,
                local_sha,
            },
        }))
    }

    async fn refresh_summary(&self, task: &Task) {
        let Some(change) = task
            .change_request
            .as_ref()
            .filter(|c| c.state == ChangeState::Open)
            .filter(|_| task.external.is_none() && task.revert.is_none())
        else {
            return;
        };
        let refreshed = async {
            let repo = self.repo_ref(task)?;
            let block = self.summary_block(task).map_err(internal)?;
            let current = self.ports.scm.change_summary(&repo, change.number).await?;
            let Some(body) = summary::replace_block(&current.body, &block) else {
                return Ok(());
            };
            if body == current.body {
                return Ok(());
            }
            self.ports
                .scm
                .update_change_request(&repo, change.number, &body)
                .await
        }
        .await;
        if let Err(e) = refreshed {
            tracing::warn!(
                work_item_id = %task.item.id,
                number = change.number,
                code = %e.code,
                "delivery.change_summary.update_failed"
            );
        }
    }

    pub(super) async fn open_change(&self, task: &Task) -> Execution {
        if !task.is_writing_at(StagePhase::OpeningChange) || task.change_request.is_some() {
            return Ok(Effect::Skipped);
        }
        let base_branch = self.base_branch(task)?;
        let (title, body) = self.change_text(task).map_err(internal)?;
        let request = OpenChangeRequest {
            repo: self.repo_ref(task)?,
            head_branch: task.item.branch.clone(),
            base_branch: base_branch.clone(),
            title,
            body,
            credential: ChangeCredential::App,
        };
        let (opened, opened_with) = match self.open_with(task, request).await? {
            Ok(opened) => opened,
            Err(block) => {
                return Ok(Effect::Fact(TaskInput::CredentialBlocked {
                    block,
                    target: ChangeRole::Code,
                }));
            }
        };
        Ok(Effect::Fact(TaskInput::ChangeOpened {
            role: ChangeRole::Code,
            change_request_id: ChangeRequestId(task.item.id.0),
            number: opened.number,
            url: opened.url,
            head_sha: opened.head_sha,
            base_branch,
            opened_with,
        }))
    }

    pub(super) async fn open_docs_change(&self, task: &Task, job: uuid::Uuid) -> Execution {
        let docs = task.environment.as_ref().and_then(|e| e.docs.as_ref());
        let (Some(docs), Some(code)) = (docs, task.change_request.as_ref()) else {
            return Ok(Effect::Skipped);
        };
        if !task.is_writing_at(StagePhase::OpeningChange)
            || !saka_core::task::needs_docs_change(task)
        {
            return Ok(Effect::Skipped);
        }
        let (project, _) = self.bound(task)?;
        let code_title = text::change_title(&task.item.work);
        let title: String = format!("docs: {code_title}")
            .chars()
            .take(DOCS_TITLE_MAX)
            .collect();
        let body = format!(
            "Docs for {}\n\nSaka-Task: {}\n",
            code.url, task.item.task_key
        );
        let request = OpenChangeRequest {
            repo: self.repo_ref_of(docs.repo_id)?,
            head_branch: docs.branch.clone().unwrap_or_default(),
            base_branch: project.base_branch.clone(),
            title,
            body,
            credential: ChangeCredential::App,
        };
        let (opened, opened_with) = match self.open_with(task, request).await? {
            Ok(opened) => opened,
            Err(block) => {
                return Ok(Effect::Fact(TaskInput::CredentialBlocked {
                    block,
                    target: ChangeRole::Docs,
                }));
            }
        };
        Ok(Effect::Fact(TaskInput::ChangeOpened {
            role: ChangeRole::Docs,
            change_request_id: ChangeRequestId(job),
            number: opened.number,
            url: opened.url,
            head_sha: opened.head_sha,
            base_branch: project.base_branch,
            opened_with,
        }))
    }

    pub(super) async fn open_with(
        &self,
        task: &Task,
        mut request: OpenChangeRequest,
    ) -> std::result::Result<
        std::result::Result<(saka_ports::OpenedChange, ChangeCredentialKind), CredentialBlock>,
        PortError,
    > {
        let integration = request.repo.integration_id.clone();
        let opener = match self.change_credential(task, &integration).await? {
            Ok(opener) => opener,
            Err(block) => return Ok(Err(block)),
        };
        request.credential = opener.credential;
        let opened = match self.ports.scm.open_change_request(&request).await? {
            ChangeOpening::Opened(opened) => opened,
            ChangeOpening::CredentialRejected => {
                let block = match &opener.by {
                    OpenedBy::Token { set_at, .. } => {
                        self.invalidate_user_token(
                            task.item.project_id,
                            *set_at,
                            TokenInvalidCause::Rejected,
                        )
                        .map_err(internal)?;
                        CredentialBlock::TokenInvalid {
                            cause: UserTokenCause::Rejected,
                            phase: StagePhase::OpeningChange,
                        }
                    }
                    OpenedBy::Link(link) => CredentialBlock::LinkRequired {
                        integration_id: integration,
                        cause: self.revoke_user_link(link)?,
                        phase: StagePhase::OpeningChange,
                    },
                    OpenedBy::App => {
                        return Err(PortError::permanent(
                            "scm_app_auth_refused",
                            json!({ "status": 401 }),
                        ));
                    }
                };
                return Ok(Err(block));
            }
            ChangeOpening::AccessDenied { status } => {
                return Ok(Err(CredentialBlock::AccessDenied {
                    credential: opener.by.kind(),
                    login: opener.by.login(),
                    status,
                }));
            }
        };
        Ok(Ok((opened, opener.by.kind())))
    }

    pub(super) async fn poll(&self, task: &Task, cr: ChangeRequestId) -> Execution {
        if let Some(docs) = task
            .docs_change
            .as_ref()
            .filter(|c| c.id == cr && saka_core::task::watches_docs_change(task))
        {
            let snapshot = self
                .ports
                .scm
                .change_request(&self.repo_ref_of(docs.repo_id)?, docs.number)
                .await?;
            return Ok(Effect::Fact(TaskInput::ChangeObserved {
                role: ChangeRole::Docs,
                state: snapshot.state,
                head_sha: snapshot.head_sha,
                merge_commit_sha: snapshot.merge_commit_sha,
                merged_at: snapshot.merged_at,
                checks: Vec::new(),
            }));
        }
        let Some(change) = task
            .change_request
            .as_ref()
            .filter(|c| c.id == cr && saka_core::task::watches_change(task))
        else {
            return Ok(Effect::Skipped);
        };
        if task.item.origin == WorkItemOrigin::ExternalChange {
            return self.poll_external(task, change.number).await;
        }
        let snapshot = self
            .ports
            .scm
            .change_request(&self.repo_ref(task)?, change.number)
            .await?;
        if snapshot.state == ChangeState::Merged
            && matches!(task.item.stage, Some(Stage::Dev | Stage::Fix))
            && let Some(docs_head_sha) = saka_core::task::docs_pending(task)
        {
            tracing::info!(
                work_item_id = %task.item.id,
                docs_head_sha,
                "delivery.docs_change.discarded"
            );
        }
        let native = task.is_at(Stage::Merge, StagePhase::Merging)
            && task.stage_run.as_ref().is_some_and(|r| r.queue_native);
        if native && snapshot.state == ChangeState::Open {
            let repo = self.repo_ref(task)?;
            if !self.ports.scm.in_merge_queue(&repo, change.number).await? {
                return Ok(Effect::Fact(TaskInput::QueueLeft));
            }
        }
        Ok(Effect::Fact(TaskInput::ChangeObserved {
            role: ChangeRole::Code,
            state: snapshot.state,
            head_sha: snapshot.head_sha,
            merge_commit_sha: snapshot.merge_commit_sha,
            merged_at: snapshot.merged_at,
            checks: snapshot.checks,
        }))
    }

    pub(super) async fn poll_external(&self, task: &Task, number: u64) -> Execution {
        let repo = self.repo_ref(task)?;
        let summary = self.ports.scm.change_summary(&repo, number).await?;
        let (merge_commit_sha, merged_at) = if summary.state == ChangeState::Merged {
            let merged = self.ports.scm.change_request(&repo, number).await?;
            (merged.merge_commit_sha, merged.merged_at)
        } else {
            (None, None)
        };
        let checks = if summary.state == ChangeState::Open && task.is_at_stage(Stage::Merge) {
            self.ports
                .scm
                .commit_checks(&repo, &summary.head_sha)
                .await?
        } else {
            Vec::new()
        };
        Ok(Effect::Fact(TaskInput::ChangeObserved {
            role: ChangeRole::Code,
            state: summary.state,
            head_sha: summary.head_sha,
            merge_commit_sha,
            merged_at,
            checks,
        }))
    }

    pub(super) async fn poll_commit(&self, task: &Task, sha: String) -> Execution {
        let targeted = task.is_at(Stage::Sync, StagePhase::Verifying)
            && task
                .stage_run
                .as_ref()
                .is_some_and(|r| r.head_sha.as_deref() == Some(sha.as_str()));
        if !targeted {
            return Ok(Effect::Skipped);
        }
        let checks = self
            .ports
            .scm
            .commit_checks(&self.repo_ref(task)?, &sha)
            .await?;
        Ok(Effect::Fact(TaskInput::CommitChecksObserved {
            head_sha: sha,
            checks,
        }))
    }
}
