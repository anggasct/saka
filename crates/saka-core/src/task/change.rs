use serde_json::json;

use super::queries::{docs_open, docs_pending, merge_status, needs_docs_change, watches_change};
use super::{Decider, Opened, with_target};
use crate::Ctx;
use crate::authorship::CredentialBlock;
use crate::event::Event;
use crate::external;
use crate::gate::Check;
use crate::ids::GateResultId;
use crate::time::Timestamp;
use crate::vocab::{
    BlockerCode, BlockerResolution, ChangeRole, ChangeState, ExternalCause, GateSource,
    SessionState, Stage, StagePhase, StageRunState, Trigger, WorkItemOrigin, WorkItemState,
};

impl Decider<'_> {
    pub(super) fn branch_pushed(&self, sha: String, ctx: &mut Ctx<'_>) -> Vec<Event> {
        if !self.task.is_writing_at(StagePhase::Pushing) || docs_pending(self.task).is_some() {
            return vec![];
        }
        let mut events = vec![Event::BranchPushed {
            work_item_id: self.item().id,
            target: ChangeRole::Code,
            branch: self.item().branch.clone(),
            sha: sha.clone(),
        }];
        match &self.task.change_request {
            Some(cr) if cr.state == ChangeState::Open => {
                if cr.head_sha != sha {
                    events.push(Event::ChangeRequestUpdated {
                        work_item_id: self.item().id,
                        change_request_id: cr.id,
                        role: ChangeRole::Code,
                        head_sha: sha.clone(),
                    });
                }
                if needs_docs_change(self.task) {
                    events.extend(self.phase(StagePhase::OpeningChange, Some(sha)));
                } else {
                    events.extend(self.dev_done(sha, ctx));
                }
            }
            _ => events.extend(self.phase(StagePhase::OpeningChange, Some(sha))),
        }
        events
    }

    // Docs go first: the code push that follows is what moves the stage on.
    pub(super) fn docs_pushed(&self, sha: String) -> Vec<Event> {
        let branch = self
            .task
            .environment
            .as_ref()
            .and_then(|e| e.docs.as_ref())
            .and_then(|d| d.branch.clone());
        let (true, Some(branch)) = (self.task.is_writing_at(StagePhase::Pushing), branch) else {
            return vec![];
        };
        let mut events = vec![Event::BranchPushed {
            work_item_id: self.item().id,
            target: ChangeRole::Docs,
            branch,
            sha: sha.clone(),
        }];
        if let Some(cr) = self
            .task
            .docs_change
            .as_ref()
            .filter(|c| c.state == ChangeState::Open && c.head_sha != sha)
        {
            events.push(Event::ChangeRequestUpdated {
                work_item_id: self.item().id,
                change_request_id: cr.id,
                role: ChangeRole::Docs,
                head_sha: sha,
            });
        }
        events
    }

    pub(super) fn dev_done(&self, head_sha: String, ctx: &mut Ctx<'_>) -> Vec<Event> {
        let Some(run) = &self.task.stage_run else {
            return vec![];
        };
        let next = if self.task.external.is_some() {
            external::review_run(
                self.item().id,
                self.next_attempt(Stage::Review),
                &head_sha,
                ExternalCause::Auto,
                None,
                ctx,
            )
        } else {
            self.new_run(
                Stage::Merge,
                Trigger::Auto,
                StagePhase::AwaitingCi,
                None,
                None,
                ctx,
            )
        };
        vec![
            Event::StageRunSucceeded {
                work_item_id: self.item().id,
                stage_run_id: run.id,
                head_sha: Some(head_sha),
            },
            next,
        ]
    }

    pub(super) fn change_opened(&self, opened: Opened, ctx: &mut Ctx<'_>) -> Vec<Event> {
        let Opened {
            role,
            id,
            number,
            url,
            head_sha,
            base_branch,
            opened_with,
        } = opened;
        if !self.task.is_writing_at(StagePhase::OpeningChange) {
            return vec![];
        }
        if role == ChangeRole::Docs {
            let docs = self.task.environment.as_ref().and_then(|e| e.docs.as_ref());
            let (Some(docs), Some(code), None) = (
                docs,
                self.task.change_request.as_ref(),
                &self.task.docs_change,
            ) else {
                return vec![];
            };
            let mut events = vec![Event::ChangeRequestOpened {
                work_item_id: self.item().id,
                change_request_id: id,
                role,
                repo_id: docs.repo_id,
                number,
                url,
                head_branch: docs.branch.clone().unwrap_or_default(),
                base_branch,
                head_sha,
                opened_with,
            }];
            let code_head = self
                .task
                .stage_run
                .as_ref()
                .and_then(|r| r.head_sha.clone())
                .unwrap_or_else(|| code.head_sha.clone());
            events.extend(self.dev_done(code_head, ctx));
            return events;
        }
        if self.task.change_request.is_some() {
            return vec![];
        }
        let mut events = vec![Event::ChangeRequestOpened {
            work_item_id: self.item().id,
            change_request_id: id,
            role,
            repo_id: self.item().repo_id,
            number,
            url,
            head_branch: self.item().branch.clone(),
            base_branch,
            head_sha: head_sha.clone(),
            opened_with,
        }];
        if needs_docs_change(self.task) {
            events.extend(self.phase(StagePhase::OpeningChange, Some(head_sha)));
        } else {
            events.extend(self.dev_done(head_sha, ctx));
        }
        events
    }

    pub(super) fn credential_blocked(
        &self,
        block: &CredentialBlock,
        target: ChangeRole,
        ctx: &mut Ctx<'_>,
    ) -> Vec<Event> {
        let phase = block.phase();
        let waiting = self.task.is_writing_at(phase)
            && (phase != StagePhase::RunningSession || self.task.live_session().is_none());
        if !waiting {
            return vec![];
        }
        let params = match target {
            ChangeRole::Docs => with_target(block.params()),
            ChangeRole::Code => block.params(),
        };
        self.block(block.code(), params, ctx)
    }

    pub(super) fn docs_observed(
        &self,
        state: ChangeState,
        head_sha: String,
        merge_commit_sha: Option<String>,
        merged_at: Option<Timestamp>,
        by_saka: bool,
    ) -> Vec<Event> {
        let Some(cr) = self
            .task
            .docs_change
            .as_ref()
            .filter(|c| c.state == ChangeState::Open)
        else {
            return vec![];
        };
        if self.item().state.is_terminal() {
            return vec![];
        }
        let id = self.item().id;
        let mut events = match state {
            ChangeState::Open if !head_sha.is_empty() && head_sha != cr.head_sha => {
                return vec![Event::ChangeRequestUpdated {
                    work_item_id: id,
                    change_request_id: cr.id,
                    role: ChangeRole::Docs,
                    head_sha,
                }];
            }
            ChangeState::Open => return vec![],
            ChangeState::Closed => vec![
                Event::ChangeRequestClosed {
                    work_item_id: id,
                    change_request_id: cr.id,
                    role: ChangeRole::Docs,
                },
                Event::DocsChangeSkipped {
                    work_item_id: id,
                    change_request_id: cr.id,
                    number: cr.number,
                    reason: "closed".into(),
                },
            ],
            ChangeState::Merged => {
                let Some(merge_commit_sha) = merge_commit_sha else {
                    return vec![];
                };
                vec![Event::ChangeRequestMerged {
                    work_item_id: id,
                    change_request_id: cr.id,
                    role: ChangeRole::Docs,
                    merge_commit_sha,
                    merged_at,
                    merged_by_saka: by_saka,
                }]
            }
        };
        if self.task.is_at(Stage::Sync, StagePhase::MergingDocs) {
            events.extend(self.phase(StagePhase::Verifying, None));
        }
        events
    }

    pub(super) fn docs_merge_rejected(&self, reason: String, ctx: &mut Ctx<'_>) -> Vec<Event> {
        let Some(cr) = self
            .task
            .docs_change
            .as_ref()
            .filter(|_| self.task.is_at(Stage::Sync, StagePhase::MergingDocs))
        else {
            return vec![];
        };
        let status = merge_status(&reason);
        self.block(
            BlockerCode::DocsMergeFailed,
            json!({ "reason": reason, "number": cr.number, "head_sha": cr.head_sha, "status": status }),
            ctx,
        )
    }

    pub(super) fn sync_entry(&self) -> StagePhase {
        if docs_open(self.task) {
            StagePhase::MergingDocs
        } else {
            StagePhase::Verifying
        }
    }

    pub(super) fn change_observed(
        &self,
        state: ChangeState,
        head_sha: String,
        merge_commit_sha: Option<String>,
        merged_at: Option<Timestamp>,
        checks: Vec<Check>,
        ctx: &mut Ctx<'_>,
    ) -> Vec<Event> {
        let in_merge = [
            StagePhase::Queued,
            StagePhase::Integrating,
            StagePhase::AwaitingCi,
            StagePhase::AwaitingMerge,
            StagePhase::Merging,
        ]
        .iter()
        .any(|p| self.task.is_at(Stage::Merge, *p));
        let (Some(cr), Some(run)) = (&self.task.change_request, &self.task.stage_run) else {
            return vec![];
        };
        if cr.state != ChangeState::Open {
            return vec![];
        }
        if !in_merge {
            return match (state, merge_commit_sha) {
                (ChangeState::Merged, Some(sha)) if watches_change(self.task) => {
                    self.merged_elsewhere(sha, merged_at, ctx)
                }
                (ChangeState::Closed, _) if self.reviewing_ours() => self.closed_in_review(ctx),
                (ChangeState::Open, _) if head_sha != cr.head_sha => self.head_moved(head_sha, ctx),
                _ => vec![],
            };
        }
        match state {
            ChangeState::Merged => {
                let Some(sha) = merge_commit_sha else {
                    return vec![];
                };
                return self.merged(sha, merged_at, false, ctx);
            }
            ChangeState::Closed => {
                let mut events = vec![Event::ChangeRequestClosed {
                    work_item_id: self.item().id,
                    change_request_id: cr.id,
                    role: ChangeRole::Code,
                }];
                events.extend(self.block(
                    BlockerCode::ChangeClosed,
                    json!({ "number": cr.number }),
                    ctx,
                ));
                return events;
            }
            ChangeState::Open => {}
        }
        let head_changed = cr.head_sha != head_sha;
        if head_changed && self.task.is_at(Stage::Merge, StagePhase::Merging) && !run.queue_native {
            return self.leave_queue(head_sha);
        }
        let mut events = Vec::new();
        if head_changed {
            events.push(Event::ChangeRequestUpdated {
                work_item_id: self.item().id,
                change_request_id: cr.id,
                role: ChangeRole::Code,
                head_sha: head_sha.clone(),
            });
        }
        let holding = self.task.is_queued()
            && (self.task.is_at(Stage::Merge, StagePhase::AwaitingCi)
                || self.task.is_at(Stage::Merge, StagePhase::AwaitingMerge));
        if head_changed && (holding || self.task.is_at(Stage::Merge, StagePhase::Queued)) {
            events.push(Event::MergeDequeued {
                work_item_id: self.item().id,
                reason: "head_moved".to_string(),
            });
            if !holding {
                events.extend(self.phase(StagePhase::AwaitingCi, Some(head_sha)));
                return events;
            }
        }
        let unchanged = self
            .task
            .gate_for(GateSource::Scm, &head_sha)
            .is_some_and(|g| g.checks == checks);
        let policy = self.gate_policy();
        let outcome = policy.scm_outcome(&checks);
        if !unchanged {
            events.push(Event::GateObserved {
                work_item_id: self.item().id,
                stage_run_id: run.id,
                gate_result_id: GateResultId::new(ctx.ids),
                source: GateSource::Scm,
                head_sha: head_sha.clone(),
                outcome,
                checks: checks.clone(),
            });
        }
        events.extend(self.settle_gate(
            &head_sha,
            head_changed,
            Some((outcome, &checks)),
            None,
            ctx,
        ));
        events
    }

    pub(super) fn reviewing_ours(&self) -> bool {
        self.task.item.origin == WorkItemOrigin::SpecStep
            && self.item().state == WorkItemState::InStage
            && self.item().stage == Some(Stage::Review)
    }

    pub(super) fn closed_in_review(&self, ctx: &mut Ctx<'_>) -> Vec<Event> {
        let Some(cr) = &self.task.change_request else {
            return vec![];
        };
        let mut events = vec![Event::ChangeRequestClosed {
            work_item_id: self.item().id,
            change_request_id: cr.id,
            role: ChangeRole::Code,
        }];
        events.extend(self.end_session(SessionState::Ended, "change_closed", json!({})));
        events.extend(self.block(
            BlockerCode::ChangeClosed,
            json!({ "number": cr.number }),
            ctx,
        ));
        events
    }

    pub(super) fn head_moved(&self, head_sha: String, ctx: &mut Ctx<'_>) -> Vec<Event> {
        let (Some(cr), Some(run)) = (&self.task.change_request, &self.task.stage_run) else {
            return vec![];
        };
        if run.head_sha.as_deref() == Some(head_sha.as_str()) {
            return vec![];
        }
        let mut events = vec![Event::ChangeRequestUpdated {
            work_item_id: self.item().id,
            change_request_id: cr.id,
            role: ChangeRole::Code,
            head_sha: head_sha.clone(),
        }];
        if self.reviewing_ours() {
            events.extend(self.end_session(SessionState::Ended, "head_moved", json!({})));
            if matches!(run.state, StageRunState::Pending | StageRunState::Running) {
                events.push(Event::StageRunCancelled {
                    work_item_id: self.item().id,
                    stage_run_id: run.id,
                    reason: "head_moved".to_string(),
                });
            }
            events.push(self.new_run(
                Stage::Merge,
                Trigger::Auto,
                StagePhase::AwaitingCi,
                None,
                None,
                ctx,
            ));
        } else if external::merging(self.task) {
            if let Some(blocker) = self.task.open_blocker() {
                events.push(Event::BlockerResolved {
                    work_item_id: self.item().id,
                    blocker_id: blocker.id,
                    resolution: BlockerResolution::Auto,
                    reason: Some("head_moved".to_string()),
                });
            }
            events.push(self.new_run(
                Stage::Merge,
                Trigger::Auto,
                StagePhase::AwaitingCi,
                Some("head_moved".to_string()),
                None,
                ctx,
            ));
        } else if self.task.is_writing() {
            events.extend(self.end_session(SessionState::Ended, "branch_moved", json!({})));
            events.extend(self.block(
                BlockerCode::BranchMoved,
                json!({ "number": cr.number, "head_sha": head_sha, "previous_head_sha": cr.head_sha }),
                ctx,
            ));
        }
        events
    }
}
