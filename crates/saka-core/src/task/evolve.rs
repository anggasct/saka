use super::queries::count_retry;
use crate::event::Event;
use crate::model::{
    Blocker, ChangeRequest, Coded, ExternalChange, GateResult, Review, ReviewPosted, StageRun,
    Task, Usage, WorkItem,
};
use crate::session;
use crate::time::Timestamp;
use crate::vocab::{
    ChangeRole, ChangeState, GateOutcome, ResumeCause, Stage, StagePhase, StageRunState,
    WorkItemOrigin, WorkItemState,
};
use crate::workflow::Workflow;

pub fn evolve(task: Option<Task>, event: &Event, now: Timestamp) -> Option<Task> {
    if let Event::WorkItemCreated {
        work_item_id,
        project_id,
        spec_id,
        spec_revision_id,
        step_id,
        step_index,
        task_key,
        work,
        branch,
        repo_id,
        priority,
    } = event
    {
        return Some(Task::new(WorkItem {
            id: *work_item_id,
            project_id: *project_id,
            origin: WorkItemOrigin::SpecStep,
            spec_id: Some(*spec_id),
            spec_revision_id: Some(*spec_revision_id),
            step_id: Some(step_id.clone()),
            step_index: Some(*step_index),
            task_key: task_key.clone(),
            work: work.clone(),
            repo_id: *repo_id,
            branch: branch.clone(),
            state: WorkItemState::Queued,
            stage: None,
            stage_phase: None,
            priority: *priority,
            version: 1,
            admitted_at: None,
            done_at: None,
            start_approved_at: None,
            created_at: now,
            updated_at: now,
        }));
    }
    if let Event::ExternalChangeAccepted {
        work_item_id,
        project_id,
        repo_id,
        task_key,
        title,
        branch,
        change_request_id,
        number,
        url,
        base_branch,
        head_sha,
        author,
        from_fork,
        draft,
        spec,
        cause,
        actor,
    } = event
    {
        let mut t = Task::new(WorkItem {
            id: *work_item_id,
            project_id: *project_id,
            origin: WorkItemOrigin::ExternalChange,
            spec_id: None,
            spec_revision_id: None,
            step_id: None,
            step_index: None,
            task_key: task_key.clone(),
            work: title.clone(),
            repo_id: *repo_id,
            branch: branch.clone(),
            state: WorkItemState::InStage,
            stage: Some(Stage::Review),
            stage_phase: Some(StagePhase::AwaitingTrigger),
            priority: 0,
            version: 1,
            admitted_at: None,
            done_at: None,
            start_approved_at: None,
            created_at: now,
            updated_at: now,
        });
        t.change_request = Some(ChangeRequest {
            id: *change_request_id,
            work_item_id: *work_item_id,
            role: ChangeRole::Code,
            repo_id: *repo_id,
            number: *number,
            url: url.clone(),
            head_branch: branch.clone(),
            base_branch: base_branch.clone(),
            head_sha: head_sha.clone(),
            state: ChangeState::Open,
            merge_commit_sha: None,
            merged_at: None,
            skipped_at: None,
            last_polled_at: None,
            created_at: now,
            updated_at: now,
        });
        t.external = Some(ExternalChange {
            title: title.clone(),
            author: author.clone(),
            from_fork: *from_fork,
            draft: *draft,
            spec: spec.clone(),
            cause: *cause,
            actor: actor.clone(),
            created_at: now,
            spec_slug: None,
            workflow: Workflow::default(),
        });
        return Some(t);
    }
    let mut t = task?;
    if event.work_item_id() != Some(t.item.id) {
        return Some(t);
    }
    t.item.version += 1;
    t.item.updated_at = now;
    apply(&mut t, event, now);
    Some(t)
}

pub(super) fn set_place(
    item: &mut WorkItem,
    state: WorkItemState,
    place: Option<(Stage, StagePhase)>,
) {
    item.state = state;
    item.stage = place.map(|p| p.0);
    item.stage_phase = place.map(|p| p.1);
}

pub(super) fn apply(t: &mut Task, event: &Event, now: Timestamp) {
    use Event as E;
    if let E::SessionStarted {
        owner,
        resume_cause: Some(ResumeCause::Lost),
        ..
    } = event
        && let Some(run) = t
            .stage_run
            .as_mut()
            .filter(|r| owner.stage_run_id() == Some(r.id))
    {
        run.resumed_lost = true;
    }
    if session::apply(t.run_parts(), event, now) {
        return;
    }
    match event {
        E::StageRunCancelled { stage_run_id, .. } => {
            let mut fixing = false;
            if let Some(run) = t.stage_run.as_mut().filter(|r| r.id == *stage_run_id)
                && matches!(run.state, StageRunState::Pending | StageRunState::Running)
            {
                run.state = StageRunState::Cancelled;
                run.ended_at = Some(now);
                run.updated_at = now;
                fixing = run.stage == Stage::Fix;
            }
            if t.item.state == WorkItemState::InStage && t.item.stage == Some(Stage::Review) {
                t.item.stage_phase = Some(StagePhase::AwaitingTrigger);
            } else if fixing && t.external.is_some() {
                set_place(
                    &mut t.item,
                    WorkItemState::InStage,
                    Some((Stage::Review, StagePhase::AwaitingTrigger)),
                );
            }
        }
        E::WorkItemRepinned {
            spec_revision_id,
            step_index,
            work,
            repo_id,
            priority,
            restored,
            ..
        } => {
            t.item.spec_revision_id = Some(*spec_revision_id);
            t.item.step_index = Some(*step_index);
            t.item.work.clone_from(work);
            t.item.repo_id = *repo_id;
            t.item.priority = *priority;
            if *restored {
                set_place(&mut t.item, WorkItemState::Queued, None);
            }
        }
        E::WorkItemSuperseded { .. } => {
            set_place(&mut t.item, WorkItemState::Superseded, None);
        }
        E::WorkItemAdmitted { .. } => {
            t.item.state = WorkItemState::Admitted;
            t.item.admitted_at = Some(now);
        }
        E::WorkItemAwaitingStart { .. } => {
            t.item.state = WorkItemState::AwaitingStart;
        }
        E::WorkItemStartApproved { .. } => {
            t.item.state = WorkItemState::Queued;
            t.item.start_approved_at = Some(now);
        }
        E::StageRunCreated {
            stage_run_id,
            stage,
            attempt,
            trigger,
            entry_phase,
            reason,
            budget,
            head_sha,
            fix,
            ..
        } => {
            count_retry(&mut t.retries, *stage, *trigger, reason.as_deref());
            if let Some(mut old) = t.stage_run.take() {
                if old.stage == Stage::Review && old.state == StageRunState::Pending {
                    old.state = StageRunState::Cancelled;
                    old.ended_at = Some(now);
                    old.updated_at = now;
                }
                t.retired_runs.push(old);
            }
            t.prior_usage = Usage::default();
            let queued = *entry_phase == StagePhase::AwaitingReview;
            let waiting = *entry_phase == StagePhase::PreparingEnvironment || queued;
            t.stage_run = Some(StageRun {
                id: *stage_run_id,
                work_item_id: t.item.id,
                stage: *stage,
                attempt: *attempt,
                state: if waiting {
                    StageRunState::Pending
                } else {
                    StageRunState::Running
                },
                trigger: *trigger,
                reason: reason.clone(),
                entry_phase: *entry_phase,
                head_sha: head_sha.clone(),
                docs_head_sha: None,
                queued_at: None,
                queue_native: false,
                fix: fix.clone(),
                failure: None,
                budget: *budget,
                budget_warnings: Vec::new(),
                resumed_lost: false,
                started_at: if waiting { None } else { Some(now) },
                ended_at: None,
                created_at: now,
                updated_at: now,
            });
            t.attempts.insert(*stage, *attempt);
            let merged_head = t.change_request.as_ref().map(|c| c.head_sha.clone());
            match (*stage, *entry_phase) {
                // The review judges the head the gates passed on, and the merge and the sync that
                // follow it need those gates unchanged, whichever run recorded them.
                (Stage::Sync | Stage::Review, _) | (Stage::Merge, StagePhase::AwaitingMerge) => {
                    t.gates
                        .retain(|g| Some(&g.head_sha) == merged_head.as_ref());
                }
                _ => t.gates.clear(),
            }
            if queued {
                set_place(
                    &mut t.item,
                    WorkItemState::InStage,
                    Some((*stage, *entry_phase)),
                );
            } else if waiting {
                set_place(&mut t.item, WorkItemState::Admitted, None);
            } else {
                set_place(
                    &mut t.item,
                    WorkItemState::InStage,
                    Some((*stage, *entry_phase)),
                );
            }
        }
        E::StagePhaseChanged {
            phase,
            head_sha,
            docs_head_sha,
            ..
        } => {
            if let Some(run) = &mut t.stage_run {
                if run.stage == Stage::Sync && head_sha.is_some() && run.head_sha != *head_sha {
                    t.gates.clear();
                }
                run.state = StageRunState::Running;
                run.started_at.get_or_insert(now);
                if head_sha.is_some() {
                    run.head_sha.clone_from(head_sha);
                }
                if docs_head_sha.is_some() {
                    run.docs_head_sha.clone_from(docs_head_sha);
                }
                run.updated_at = now;
                let stage = run.stage;
                set_place(&mut t.item, WorkItemState::InStage, Some((stage, *phase)));
            }
        }
        E::StageRunSucceeded {
            stage_run_id,
            head_sha,
            ..
        } => {
            if let Some(run) = t.stage_run.as_mut().filter(|r| r.id == *stage_run_id) {
                run.state = StageRunState::Succeeded;
                if head_sha.is_some() {
                    run.head_sha.clone_from(head_sha);
                }
                run.ended_at = Some(now);
                run.updated_at = now;
            }
        }
        E::GateObserved {
            stage_run_id,
            gate_result_id,
            source,
            head_sha,
            outcome,
            checks,
            ..
        } => {
            t.gates.retain(|g| g.source != *source);
            t.gates.insert(
                t.gates.partition_point(|g| g.source < *source),
                GateResult {
                    id: *gate_result_id,
                    stage_run_id: *stage_run_id,
                    work_item_id: t.item.id,
                    source: *source,
                    head_sha: head_sha.clone(),
                    outcome: *outcome,
                    checks: checks.clone(),
                    observed_at: now,
                },
            );
        }
        E::GateStarted {
            stage_run_id,
            gate_result_id,
            source,
            head_sha,
            ..
        } => {
            t.gates.retain(|g| g.source != *source);
            t.gates.insert(
                t.gates.partition_point(|g| g.source < *source),
                GateResult {
                    id: *gate_result_id,
                    stage_run_id: *stage_run_id,
                    work_item_id: t.item.id,
                    source: *source,
                    head_sha: head_sha.clone(),
                    outcome: GateOutcome::Pending,
                    checks: Vec::new(),
                    observed_at: now,
                },
            );
        }
        E::MergeRequested { .. } => {
            if let Some(run) = &mut t.stage_run {
                run.queued_at.get_or_insert(now);
            }
        }
        E::MergeRebased { .. } => {}
        E::MergeEnqueued { .. } => {
            if let Some(run) = &mut t.stage_run {
                run.queue_native = true;
            }
        }
        E::MergeDequeued { .. } => {
            if let Some(run) = &mut t.stage_run {
                run.queued_at = None;
            }
        }
        E::ReviewRecorded {
            review_id,
            stage_run_id,
            session_id,
            head_sha,
            outcome,
            basis,
            summary,
            findings,
            questions,
            ..
        } => t.reviews.push(Review {
            id: *review_id,
            work_item_id: t.item.id,
            stage_run_id: *stage_run_id,
            session_id: *session_id,
            head_sha: head_sha.clone(),
            outcome: *outcome,
            basis: *basis,
            summary: summary.clone(),
            findings: findings.clone(),
            questions: questions.clone(),
            posted: None,
            created_at: now,
        }),
        E::ReviewPosted {
            review_id,
            event,
            url,
            ..
        } => {
            if let Some(review) = t.reviews.iter_mut().find(|r| r.id == *review_id) {
                review.posted = Some(ReviewPosted {
                    event: *event,
                    url: url.clone(),
                    at: now,
                });
            }
        }
        E::WorkItemBlocked {
            stage_run_id,
            blocker_id,
            code,
            params,
            ..
        } => {
            t.blocker = Some(Blocker {
                id: *blocker_id,
                work_item_id: t.item.id,
                stage_run_id: *stage_run_id,
                code: *code,
                params: params.clone(),
                resolution: None,
                opened_at: now,
                resolved_at: None,
            });
            if let Some(run) = &mut t.stage_run {
                run.state = StageRunState::Blocked;
                run.failure = Some(Coded {
                    code: code.as_str().to_string(),
                    params: params.clone(),
                });
                run.ended_at = Some(now);
                run.updated_at = now;
            }
            set_place(&mut t.item, WorkItemState::Blocked, None);
        }
        E::BlockerResolved {
            blocker_id,
            resolution,
            ..
        } => {
            if let Some(b) = t.blocker.as_mut().filter(|b| b.id == *blocker_id) {
                b.resolution = Some(*resolution);
                b.resolved_at = Some(now);
            }
        }
        E::WorkItemCancelled { .. } => {
            if let Some(run) = t
                .stage_run
                .as_mut()
                .filter(|r| matches!(r.state, StageRunState::Pending | StageRunState::Running))
            {
                run.state = StageRunState::Cancelled;
                run.ended_at = Some(now);
                run.updated_at = now;
            }
            set_place(&mut t.item, WorkItemState::Cancelled, None);
        }
        E::WorkItemTakenOver { .. } => {
            if let Some(run) = t
                .stage_run
                .as_mut()
                .filter(|r| matches!(r.state, StageRunState::Pending | StageRunState::Running))
            {
                run.state = StageRunState::Cancelled;
                run.ended_at = Some(now);
                run.updated_at = now;
            }
            set_place(&mut t.item, WorkItemState::TakenOver, None);
        }
        E::WorkItemReturned { .. } => {}
        E::WorkItemDone { .. } => {
            set_place(&mut t.item, WorkItemState::Done, None);
            t.item.done_at = Some(now);
        }
        E::BudgetWarned {
            owner, dimension, ..
        } => {
            if let Some(run) = t
                .stage_run
                .as_mut()
                .filter(|r| owner.stage_run_id() == Some(r.id))
                && !run.budget_warnings.contains(dimension)
            {
                run.budget_warnings.push(*dimension);
                run.updated_at = now;
            }
        }
        E::PermissionRuleAdded { .. } => {
            t.added_rules.extend(crate::rule::evolve(None, event, now));
        }
        E::PermissionRuleRevoked { .. } => {}
        E::BranchPushed {
            target: ChangeRole::Docs,
            sha,
            ..
        } => {
            if let Some(docs) = t.environment.as_mut().and_then(|e| e.docs.as_mut()) {
                docs.pushed_sha = Some(sha.clone());
            }
        }
        E::BranchPushed { .. } => {}
        E::ChangeRequestOpened {
            change_request_id,
            role,
            repo_id,
            number,
            url,
            head_branch,
            base_branch,
            head_sha,
            ..
        } => {
            let cr = Some(ChangeRequest {
                id: *change_request_id,
                work_item_id: t.item.id,
                role: *role,
                repo_id: *repo_id,
                number: *number,
                url: url.clone(),
                head_branch: head_branch.clone(),
                base_branch: base_branch.clone(),
                head_sha: head_sha.clone(),
                state: ChangeState::Open,
                merge_commit_sha: None,
                merged_at: None,
                skipped_at: None,
                last_polled_at: None,
                created_at: now,
                updated_at: now,
            });
            match role {
                ChangeRole::Code => t.change_request = cr,
                ChangeRole::Docs => t.docs_change = cr,
            }
        }
        E::ChangeRequestUpdated { role, head_sha, .. } => {
            if let Some(cr) = t.change_of(*role) {
                cr.head_sha.clone_from(head_sha);
                cr.updated_at = now;
            }
        }
        E::ChangeRequestMerged {
            role,
            merge_commit_sha,
            merged_at,
            ..
        } => {
            if let Some(cr) = t.change_of(*role) {
                cr.state = ChangeState::Merged;
                cr.merge_commit_sha = Some(merge_commit_sha.clone());
                cr.merged_at = merged_at.or(Some(now));
                cr.updated_at = now;
            }
            if let Some(run) = t.stage_run.as_mut().filter(|r| {
                *role == ChangeRole::Code
                    && matches!(r.stage, Stage::Dev | Stage::Fix)
                    && matches!(r.state, StageRunState::Pending | StageRunState::Running)
            }) {
                run.state = StageRunState::Cancelled;
                run.ended_at = Some(now);
                run.updated_at = now;
            }
        }
        E::ChangeRequestClosed { role, .. } => {
            if let Some(cr) = t.change_of(*role) {
                cr.state = ChangeState::Closed;
                cr.updated_at = now;
            }
        }
        E::DocsChangeSkipped { .. } => {
            if let Some(cr) = &mut t.docs_change {
                cr.skipped_at = Some(now);
                cr.updated_at = now;
            }
        }
        E::WorkItemCreated { .. }
        | E::ExternalChangeAccepted { .. }
        | E::RepoRegistered { .. }
        | E::ProjectRegistered { .. }
        | E::ProjectConfigChanged { .. }
        | E::ProjectConfigRejected { .. }
        | E::ProjectConfigErrorCleared { .. }
        | E::SecretSet { .. }
        | E::SecretRemoved { .. }
        | E::DocCommitted { .. }
        | E::SpecRegistered { .. }
        | E::SpecRevisionRecorded { .. }
        | E::SpecApproved { .. }
        | E::SpecWithdrawn { .. }
        | E::SpecShipped { .. }
        | E::JobExhausted { .. }
        | E::SpecDraftRequested { .. }
        | E::SpecDraftAttemptStarted { .. }
        | E::SpecDraftPacketRendered { .. }
        | E::SpecDraftLinted { .. }
        | E::SpecDraftAwaitingOperator { .. }
        | E::SpecDraftCommitting { .. }
        | E::SpecDraftSucceeded { .. }
        | E::SpecDraftFailed { .. }
        | E::SpecDraftRetried { .. }
        | E::SpecDraftCancelled { .. }
        | E::UserLinkAuthorizationStarted { .. }
        | E::UserLinkAuthorizationSlowed { .. }
        | E::UserLinkAuthorizationEnded { .. }
        | E::UserLinked { .. }
        | E::UserLinkRefreshed { .. }
        | E::UserLinkRelinkRequired { .. }
        | E::UserLinkRemoved { .. }
        | E::HookRunQueued { .. }
        | E::HookRunFinished { .. }
        | E::InboxItemDismissed { .. }
        | E::PairingCreated { .. }
        | E::DevicePaired { .. }
        | E::DeviceRevoked { .. }
        | E::CredentialRegistered { .. }
        | E::CredentialRevoked { .. }
        | E::LoginSessionOpened { .. }
        | E::LoginSessionClosed { .. }
        | E::ProjectAuthorshipSet { .. }
        | E::ProjectUserTokenSet { .. }
        | E::ProjectUserTokenRemoved { .. }
        | E::ProjectUserTokenInvalidated { .. } => {}
        E::EnvironmentPrepared { .. }
        | E::EnvironmentTornDown { .. }
        | E::EnvironmentTeardownFailed { .. }
        | E::SessionStarted { .. }
        | E::SessionInitialized { .. }
        | E::SessionStateChanged { .. }
        | E::SessionTurnRequested { .. }
        | E::SessionTurnEnded { .. }
        | E::SessionSteered { .. }
        | E::SessionInterruptRequested { .. }
        | E::QuotaObserved { .. }
        | E::SessionReattached { .. }
        | E::SessionStopRequested { .. }
        | E::SessionEnded { .. }
        | E::SessionLost { .. }
        | E::PermissionRequested { .. }
        | E::PermissionAnswered { .. }
        | E::PermissionDelivered { .. }
        | E::DecisionAsked { .. }
        | E::DecisionAnswered { .. }
        | E::DecisionDelivered { .. }
        | E::DecisionExpired { .. } => {}
    }
}
