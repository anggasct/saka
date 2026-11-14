use serde_json::{Value, json};

use super::queries::{gates_clear, merge_ready};
use super::{
    BranchUpdate, Decider, Integration, MAX_CONFLICT_PATHS, MERGED_ON_HOST, QUEUE_REASON, Verdict,
};
use crate::Ctx;
use crate::event::Event;
use crate::model::{Coded, DeployGate};
use crate::rejection::Rejection;
use crate::time::Timestamp;
use crate::vocab::{
    BlockerCode, BlockerResolution, ChangeRole, ChangeState, EnvironmentState, GateOutcome,
    GateSource, HookRunState, SessionState, Stage, StagePhase, StageRunState, Trigger,
    WorkItemState,
};

impl Decider<'_> {
    pub(super) fn request_merge(
        &self,
        expected_version: u64,
        reason: Option<String>,
    ) -> Result<Vec<Event>, Rejection> {
        if expected_version != self.item().version {
            return Err(Rejection::VersionStale);
        }
        let in_merge = match self.item().state {
            WorkItemState::InStage => self.item().stage == Some(Stage::Merge),
            WorkItemState::Blocked => self
                .task
                .stage_run
                .as_ref()
                .is_some_and(|r| r.stage == Stage::Merge),
            _ => false,
        };
        if !in_merge || self.task.is_at(Stage::Merge, StagePhase::Merging) {
            if !in_merge
                && self.judging()
                && let Some(gap) = self.review_gap()
            {
                return Err(gap);
            }
            return Err(Rejection::InvalidState {
                state: self.state_name(),
            });
        }
        let cr = self.task.change_request.as_ref();
        let (Some(cr), true) = (cr, merge_ready(self.task)) else {
            if gates_clear(self.task)
                && let Some(gap) = self.review_gap()
            {
                return Err(gap);
            }
            let gate_outcome = cr
                .and_then(|c| {
                    let mut at_head = self.task.gates.iter().filter(|g| g.head_sha == c.head_sha);
                    at_head
                        .clone()
                        .find(|g| g.outcome == GateOutcome::Failed)
                        .or_else(|| at_head.find(|g| g.outcome == GateOutcome::Pending))
                        .or_else(|| self.task.gate_for(GateSource::Scm, &c.head_sha))
                })
                .map(|g| g.outcome.as_str().to_string());
            return Err(Rejection::MergeNotReady {
                reason: "gate",
                gate_outcome,
                head_sha: cr.map(|c| c.head_sha.clone()),
                review_head_sha: None,
                review_outcome: None,
            });
        };
        let mut events = vec![Event::MergeRequested {
            work_item_id: self.item().id,
            head_sha: cr.head_sha.clone(),
            merge_method: self.cfg.merge_method,
            reason,
        }];
        events.extend(self.phase(StagePhase::Queued, Some(cr.head_sha.clone())));
        Ok(events)
    }

    pub(super) fn merge_not_ready(&self) -> Vec<Event> {
        let native = self.task.stage_run.as_ref().is_some_and(|r| r.queue_native);
        let Some(cr) = self
            .task
            .change_request
            .as_ref()
            .filter(|_| self.task.is_at(Stage::Merge, StagePhase::Merging) && !native)
        else {
            return vec![];
        };
        let mut events = vec![Event::MergeDequeued {
            work_item_id: self.item().id,
            reason: "not_ready".to_string(),
        }];
        events.extend(self.phase(StagePhase::AwaitingCi, Some(cr.head_sha.clone())));
        events
    }

    pub(super) fn judging(&self) -> bool {
        matches!(
            self.item().state,
            WorkItemState::InStage | WorkItemState::Blocked
        ) && self
            .task
            .stage_run
            .as_ref()
            .is_some_and(|r| matches!(r.stage, Stage::Review | Stage::Fix))
    }

    pub(super) fn review_gap(&self) -> Option<Rejection> {
        let cr = self.task.change_request.as_ref()?;
        if !self.task.has_review_stage() || self.task.approved_at(&cr.head_sha) {
            return None;
        }
        let last = self.task.reviews.last();
        Some(Rejection::MergeNotReady {
            reason: "review",
            gate_outcome: None,
            head_sha: Some(cr.head_sha.clone()),
            review_head_sha: last.map(|r| r.head_sha.clone()),
            review_outcome: last.map(|r| r.outcome.as_str()),
        })
    }

    pub(super) fn merge_turn(&self) -> Vec<Event> {
        if !self.task.is_at(Stage::Merge, StagePhase::Queued) {
            return vec![];
        }
        self.phase(StagePhase::Integrating, None)
    }

    pub(super) fn queued_merge(&self, head_sha: &str, announce: bool) -> Vec<Event> {
        let mut events = Vec::new();
        if announce {
            events.push(Event::MergeRequested {
                work_item_id: self.item().id,
                head_sha: head_sha.to_string(),
                merge_method: self.cfg.merge_method,
                reason: Some(QUEUE_REASON.to_string()),
            });
        }
        events.extend(self.phase(StagePhase::Merging, Some(head_sha.to_string())));
        events
    }

    pub(super) fn queue_left(&self, ctx: &mut Ctx<'_>) -> Vec<Event> {
        let native = self.task.stage_run.as_ref().is_some_and(|r| r.queue_native);
        let Some(cr) = self.task.change_request.as_ref().filter(|_| native) else {
            return vec![];
        };
        if !self.task.is_at(Stage::Merge, StagePhase::Merging) || cr.state != ChangeState::Open {
            return vec![];
        }
        self.block(
            BlockerCode::MergeConflict,
            json!({ "reason": "removed_from_queue", "head_sha": cr.head_sha }),
            ctx,
        )
    }

    pub(super) fn leave_queue(&self, head_sha: String) -> Vec<Event> {
        let Some(cr) = &self.task.change_request else {
            return vec![];
        };
        let mut events = Vec::new();
        if cr.head_sha != head_sha {
            events.push(Event::ChangeRequestUpdated {
                work_item_id: self.item().id,
                change_request_id: cr.id,
                role: ChangeRole::Code,
                head_sha: head_sha.clone(),
                replaced_head_sha: None,
            });
        }
        events.push(Event::MergeDequeued {
            work_item_id: self.item().id,
            reason: "head_moved".to_string(),
        });
        events.extend(self.phase(StagePhase::AwaitingCi, Some(head_sha)));
        events
    }

    pub(super) fn integrated(&self, outcome: Integration, ctx: &mut Ctx<'_>) -> Vec<Event> {
        let Some(cr) = &self.task.change_request else {
            return vec![];
        };
        if !self.task.is_at(Stage::Merge, StagePhase::Integrating) {
            return vec![];
        }
        match outcome {
            Integration::Moved { remote_sha } => self.leave_queue(remote_sha),
            Integration::Conflict {
                head_sha,
                base_sha,
                paths,
            } => {
                let count = paths.len();
                let shown: Vec<&String> = paths.iter().take(MAX_CONFLICT_PATHS).collect();
                self.conflicted(
                    json!({
                        "reason": "rebase_conflict",
                        "head_sha": head_sha,
                        "base_sha": base_sha,
                        "base_branch": cr.base_branch,
                        "paths": shown,
                        "count": count,
                    }),
                    ctx,
                )
            }
            Integration::Enqueued { head_sha } => {
                if cr.head_sha != head_sha {
                    return self.leave_queue(cr.head_sha.clone());
                }
                let mut events = vec![Event::MergeEnqueued {
                    work_item_id: self.item().id,
                    head_sha: head_sha.clone(),
                }];
                events.extend(self.phase(StagePhase::Merging, Some(head_sha)));
                events
            }
            Integration::Merged {
                merge_commit_sha,
                merged_at,
            } => self.merged(merge_commit_sha, merged_at, true, ctx),
            Integration::Rejected { reason } => self.block(
                BlockerCode::MergeConflict,
                json!({ "reason": reason, "head_sha": cr.head_sha }),
                ctx,
            ),
            Integration::Behind {
                head_sha,
                base_sha,
                updatable,
            } => {
                if cr.head_sha != head_sha {
                    return self.leave_queue(cr.head_sha.clone());
                }
                let from_fork = self.task.external.as_ref().is_some_and(|e| e.from_fork);
                let mut events = vec![Event::MergeDequeued {
                    work_item_id: self.item().id,
                    reason: "behind_base".to_string(),
                }];
                events.extend(self.block(
                    BlockerCode::ChangeBehindBase,
                    json!({
                        "base_branch": cr.base_branch,
                        "base_sha": base_sha,
                        "head_sha": head_sha,
                        "updatable": updatable && !from_fork,
                    }),
                    ctx,
                ));
                events
            }
            Integration::Unclean { head_sha } => self.conflicted(
                json!({ "reason": "worktree_unclean", "head_sha": head_sha }),
                ctx,
            ),
            Integration::Rebased {
                from_sha,
                head_sha,
                base_sha,
            } => {
                if cr.head_sha != from_sha && cr.head_sha != head_sha {
                    return self.leave_queue(cr.head_sha.clone());
                }
                let rebased = from_sha != head_sha;
                let mut events = Vec::new();
                if cr.head_sha != head_sha {
                    events.push(Event::ChangeRequestUpdated {
                        work_item_id: self.item().id,
                        change_request_id: cr.id,
                        role: ChangeRole::Code,
                        head_sha: head_sha.clone(),
                        replaced_head_sha: Some(cr.head_sha.clone()),
                    });
                }
                if rebased {
                    events.push(Event::MergeRebased {
                        work_item_id: self.item().id,
                        from_sha,
                        head_sha: head_sha.clone(),
                        base_sha,
                        base_branch: cr.base_branch.clone(),
                    });
                }
                match self.verdict(&head_sha, None, None, !rebased) {
                    Verdict::Passed if self.needs_review(&head_sha) => {
                        events.extend(self.request_review(&head_sha, ctx))
                    }
                    Verdict::Passed => events.extend(self.queued_merge(&head_sha, rebased)),
                    Verdict::Failed(params) => {
                        events.extend(self.block(BlockerCode::CiRed, params, ctx))
                    }
                    Verdict::Pending => {
                        events.extend(self.phase(StagePhase::AwaitingCi, Some(head_sha)))
                    }
                }
                events
            }
        }
    }

    pub(super) fn branch_updated(
        &self,
        expected_head_sha: &str,
        outcome: BranchUpdate,
        ctx: &mut Ctx<'_>,
    ) -> Vec<Event> {
        let Some(cr) = self
            .task
            .change_request
            .as_ref()
            .filter(|c| c.state == ChangeState::Open)
        else {
            return vec![];
        };
        if self.task.external.is_none() || !self.task.is_at(Stage::Merge, StagePhase::AwaitingCi) {
            return vec![];
        }
        match outcome {
            BranchUpdate::HeadChanged | BranchUpdate::Unchanged => vec![],
            BranchUpdate::Conflict { head_sha, base_sha } if cr.head_sha == expected_head_sha => {
                self.block(
                    BlockerCode::MergeConflict,
                    json!({
                        "reason": "update_conflict",
                        "head_sha": head_sha,
                        "base_sha": base_sha,
                        "base_branch": cr.base_branch,
                    }),
                    ctx,
                )
            }
            BranchUpdate::Conflict { .. } => vec![],
            BranchUpdate::Unexpected { head_sha } => {
                let mut events = Vec::new();
                if cr.head_sha != head_sha {
                    events.push(Event::ChangeRequestUpdated {
                        work_item_id: self.item().id,
                        change_request_id: cr.id,
                        role: ChangeRole::Code,
                        head_sha,
                        replaced_head_sha: None,
                    });
                }
                events.push(Event::MergeDequeued {
                    work_item_id: self.item().id,
                    reason: "head_moved".to_string(),
                });
                events
            }
            BranchUpdate::Updated { head_sha } => {
                if cr.head_sha != expected_head_sha && cr.head_sha != head_sha {
                    return vec![];
                }
                let mut events = vec![Event::ChangeRequestBranchUpdated {
                    work_item_id: self.item().id,
                    change_request_id: cr.id,
                    from_head_sha: expected_head_sha.to_string(),
                    head_sha: head_sha.clone(),
                }];
                events.extend(self.request_review(&head_sha, ctx));
                events
            }
        }
    }

    pub(super) fn conflicted(&self, params: Value, ctx: &mut Ctx<'_>) -> Vec<Event> {
        let cause = Coded {
            code: BlockerCode::MergeConflict.as_str().to_string(),
            params: params.clone(),
        };
        let (Some(fix), Some(run)) = (self.start_fix(cause, ctx), &self.task.stage_run) else {
            return self.block(BlockerCode::MergeConflict, params, ctx);
        };
        vec![
            Event::MergeDequeued {
                work_item_id: self.item().id,
                reason: "merge_conflict".to_string(),
            },
            Event::StageRunCancelled {
                work_item_id: self.item().id,
                stage_run_id: run.id,
                reason: "merge_conflict".to_string(),
            },
            fix,
        ]
    }

    pub(super) fn merged(
        &self,
        merge_commit_sha: String,
        merged_at: Option<Timestamp>,
        by_saka: bool,
        ctx: &mut Ctx<'_>,
    ) -> Vec<Event> {
        let (Some(cr), Some(run)) = (&self.task.change_request, &self.task.stage_run) else {
            return vec![];
        };
        vec![
            Event::ChangeRequestMerged {
                work_item_id: self.item().id,
                change_request_id: cr.id,
                role: ChangeRole::Code,
                merge_commit_sha: merge_commit_sha.clone(),
                merged_at,
                merged_by_saka: by_saka,
            },
            Event::StageRunSucceeded {
                work_item_id: self.item().id,
                stage_run_id: run.id,
                head_sha: Some(cr.head_sha.clone()),
            },
            self.new_run(
                Stage::Sync,
                Trigger::Auto,
                self.sync_entry(),
                None,
                None,
                ctx,
            ),
        ]
    }

    pub(super) fn merged_elsewhere(
        &self,
        merge_commit_sha: String,
        merged_at: Option<Timestamp>,
        ctx: &mut Ctx<'_>,
    ) -> Vec<Event> {
        let Some(cr) = &self.task.change_request else {
            return vec![];
        };
        let mut events = Vec::new();
        if let Some(b) = self.task.open_blocker() {
            events.push(Event::BlockerResolved {
                work_item_id: self.item().id,
                blocker_id: b.id,
                resolution: BlockerResolution::Auto,
                reason: Some(MERGED_ON_HOST.to_string()),
            });
        }
        events.extend(self.end_session(SessionState::Ended, "change_merged", json!({})));
        if let Some(run) = self.task.stage_run.as_ref().filter(|r| {
            r.stage == Stage::Review
                && matches!(r.state, StageRunState::Pending | StageRunState::Running)
        }) {
            events.push(Event::StageRunCancelled {
                work_item_id: self.item().id,
                stage_run_id: run.id,
                reason: "change_merged".to_string(),
            });
        }
        events.push(Event::ChangeRequestMerged {
            work_item_id: self.item().id,
            change_request_id: cr.id,
            role: ChangeRole::Code,
            merge_commit_sha,
            merged_at,
            merged_by_saka: false,
        });
        events.push(self.new_run(
            Stage::Sync,
            Trigger::Auto,
            self.sync_entry(),
            Some(MERGED_ON_HOST.to_string()),
            None,
            ctx,
        ));
        events
    }

    pub(super) fn merge_verified(
        &self,
        found: bool,
        base_tip: Option<String>,
        docs_found: Option<bool>,
        ctx: &mut Ctx<'_>,
    ) -> Vec<Event> {
        let targeted = self
            .task
            .stage_run
            .as_ref()
            .is_some_and(|r| r.head_sha.is_some());
        if !self.task.is_at(Stage::Sync, StagePhase::Verifying) || targeted {
            return vec![];
        }
        let cr = self.task.change_request.as_ref();
        if !found {
            return self.block(
                BlockerCode::DodFailed,
                json!({
                    "reason": "not_in_base",
                    "merge_commit_sha": cr.and_then(|c| c.merge_commit_sha.clone()),
                    "base_branch": cr.map(|c| c.base_branch.clone()),
                }),
                ctx,
            );
        }
        if docs_found == Some(false) {
            let docs = self.task.docs_change.as_ref();
            return self.block(
                BlockerCode::DodFailed,
                json!({
                    "reason": "docs_not_in_base",
                    "merge_commit_sha": docs.and_then(|c| c.merge_commit_sha.clone()),
                    "base_branch": docs.map(|c| c.base_branch.clone()),
                }),
                ctx,
            );
        }
        if self.proven() {
            return self.phase(StagePhase::TearingDown, None);
        }
        let first = self.task.attempts.get(&Stage::Sync).copied().unwrap_or(1) <= 1;
        let target = if first {
            cr.and_then(|c| c.merge_commit_sha.clone())
        } else {
            base_tip.or_else(|| cr.and_then(|c| c.merge_commit_sha.clone()))
        };
        match target {
            Some(t) => self.phase(StagePhase::Verifying, Some(t)),
            None => vec![],
        }
    }

    pub(super) fn environment_torn_down(&self, teardown_failures: Vec<Value>) -> Vec<Event> {
        let Some(env) = self
            .task
            .environment
            .as_ref()
            .filter(|e| e.state != EnvironmentState::TornDown)
        else {
            return vec![];
        };
        let mut events = vec![Event::EnvironmentTornDown {
            owner: env.owner,
            environment_id: env.id,
            teardown_failures,
        }];
        events.extend(self.finish_sync());
        events
    }

    pub(super) fn teardown_failed(&self, error: Coded) -> Vec<Event> {
        let Some(env) = self.task.environment.as_ref().filter(|e| {
            !matches!(
                e.state,
                EnvironmentState::TeardownFailed | EnvironmentState::TornDown
            )
        }) else {
            return vec![];
        };
        let mut events = vec![Event::EnvironmentTeardownFailed {
            owner: env.owner,
            environment_id: env.id,
            error,
        }];
        events.extend(self.finish_sync());
        events
    }

    pub(super) fn gates_pass(&self) -> bool {
        self.task
            .hook_gates
            .iter()
            .all(|g| g.state == HookRunState::Succeeded)
            && self.task.deploy_gates.iter().all(DeployGate::cleared)
    }

    pub(super) fn settle_hook_gates(&self, ctx: &mut Ctx<'_>) -> Vec<Event> {
        let gates = &self.task.hook_gates;
        if !self.task.is_at(Stage::Sync, StagePhase::TearingDown)
            || gates.iter().any(|g| g.state == HookRunState::Pending)
        {
            return vec![];
        }
        if let Some(failed) = gates.iter().find(|g| g.state == HookRunState::Failed) {
            return self.block(
                BlockerCode::HookFailed,
                json!({
                    "hook": failed.hook,
                    "hook_run_id": failed.run_id,
                    "action": failed.action,
                    "error": failed.error,
                }),
                ctx,
            );
        }
        let torn_down = self.task.environment.as_ref().is_none_or(|e| {
            matches!(
                e.state,
                EnvironmentState::TornDown | EnvironmentState::TeardownFailed
            )
        });
        if torn_down {
            self.finish_sync()
        } else {
            vec![]
        }
    }

    pub(super) fn finish_sync(&self) -> Vec<Event> {
        if !self.task.is_at(Stage::Sync, StagePhase::TearingDown) || !self.gates_pass() {
            return vec![];
        }
        let (Some(run), Some(sha)) = (
            &self.task.stage_run,
            self.task
                .change_request
                .as_ref()
                .and_then(|c| c.merge_commit_sha.clone()),
        ) else {
            return vec![];
        };
        vec![
            Event::StageRunSucceeded {
                work_item_id: self.item().id,
                stage_run_id: run.id,
                head_sha: None,
            },
            Event::WorkItemDone {
                work_item_id: self.item().id,
                merge_commit_sha: sha,
            },
        ]
    }
}
