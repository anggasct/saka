use serde_json::json;

use super::Decider;
use crate::Ctx;
use crate::event::Event;
use crate::ids::BlockerId;
use crate::model::DeployGate;
use crate::rejection::Rejection;
use crate::vocab::{
    BlockerCode, BlockerResolution, DeployGateState, DeploymentState, Stage, StagePhase, Trigger,
    WorkItemState,
};

const SUCCEEDED_LATER: &str = "deploy_succeeded";

fn changed(task: &crate::model::Task, gate: &DeployGate, state: DeployGateState) -> Event {
    Event::DeployGateChanged {
        work_item_id: task.item.id,
        environment: gate.environment.clone(),
        state,
        deployment_id: gate.deployment.as_ref().map(|d| d.id),
        reason: gate.reason.clone(),
    }
}

impl Decider<'_> {
    fn held_by_deploy(&self) -> bool {
        self.item().state == WorkItemState::Blocked
            && self
                .task
                .open_blocker()
                .is_some_and(|b| b.code == BlockerCode::DeployUnverified)
    }

    pub(super) fn settle_deploy_gates(&self, ctx: &mut Ctx<'_>) -> Vec<Event> {
        let held = self.held_by_deploy();
        if !held && !self.task.is_at(Stage::Sync, StagePhase::TearingDown) {
            return vec![];
        }
        let mut events: Vec<Event> = self
            .task
            .deploy_gates
            .iter()
            .filter(|g| !g.cleared() && g.deployment_succeeded())
            .map(|g| changed(self.task, g, DeployGateState::Passed))
            .collect();
        let all_clear = self
            .task
            .deploy_gates
            .iter()
            .all(|g| g.cleared() || g.deployment_succeeded());
        if held {
            if all_clear {
                events.extend(
                    self.retry_resolving(Trigger::Auto, None, SUCCEEDED_LATER.to_string(), ctx)
                        .unwrap_or_default(),
                );
            }
            return events;
        }
        let failed = self
            .task
            .deploy_gates
            .iter()
            .filter(|g| g.state == DeployGateState::Pending)
            .filter_map(|g| {
                g.deployment
                    .as_ref()
                    .filter(|d| d.state == DeploymentState::Failed)
                    .map(|d| (g, d))
            })
            .min_by_key(|(g, d)| (d.finished_at, g.environment.clone()));
        if let Some((gate, deployment)) = failed {
            let error = deployment.error.clone();
            events.push(changed(self.task, gate, DeployGateState::Failed));
            events.extend(self.block(
                BlockerCode::DeployUnverified,
                json!({
                    "environment": gate.environment,
                    "deployment_id": deployment.id,
                    "phase": deployment.phase,
                    "code": error.as_ref().map(|e| e.code.clone()),
                    "on_failure": deployment.on_failure,
                }),
                ctx,
            ));
        }
        events
    }

    pub(super) fn accept_unverified(
        &self,
        blocker_id: BlockerId,
        reason: String,
        ctx: &mut Ctx<'_>,
    ) -> Result<Vec<Event>, Rejection> {
        let open =
            self.held_by_deploy() && self.task.open_blocker().is_some_and(|b| b.id == blocker_id);
        if !open {
            return Err(Rejection::BlockerNotOpen {
                blocker_id: blocker_id.0.to_string(),
            });
        }
        let mut events: Vec<Event> = self
            .task
            .deploy_gates
            .iter()
            .filter(|g| !g.cleared())
            .map(|g| {
                let mut accepted = g.clone();
                accepted.reason = Some(reason.clone());
                changed(self.task, &accepted, DeployGateState::Accepted)
            })
            .collect();
        events.extend(self.retry_resolving(
            Trigger::Auto,
            Some(BlockerResolution::Accepted),
            reason,
            ctx,
        )?);
        Ok(events)
    }
}
