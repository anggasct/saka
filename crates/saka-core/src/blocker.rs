use serde_json::{Value, json};

use crate::model::Task;
use crate::task::can_retry;
use crate::vocab::{BlockerAction, BlockerCode};

#[derive(Debug, Clone, PartialEq)]
pub struct PrimaryAction {
    pub kind: BlockerAction,
    pub params: Value,
}

impl BlockerAction {
    pub fn runs_directly(self) -> bool {
        matches!(
            self,
            BlockerAction::Retry
                | BlockerAction::Cancel
                | BlockerAction::Takeover
                | BlockerAction::UpdateBranch
        )
    }
}

pub fn designated(code: BlockerCode) -> BlockerAction {
    use BlockerAction as A;
    use BlockerCode as B;
    match code {
        B::EnvSetupFailed
        | B::EnvRecipeInvalid
        | B::ToolchainUnpinned
        | B::ToolchainUnavailable
        | B::SessionFailed
        | B::SessionLost
        | B::SessionTimeout
        | B::PermissionExpired
        | B::DecisionExpired
        | B::QuotaExhausted
        | B::BudgetExhausted
        | B::NoChanges
        | B::CiRed
        | B::VerdictFailed
        | B::ReviewRoundsExhausted
        | B::MergeConflict
        | B::PushRejected
        | B::PathScopeViolation
        | B::UserAccessDenied
        | B::ScmUnavailable
        | B::DodFailed
        | B::DocsMergeFailed
        | B::HookFailed
        | B::RegressionNotReproduced
        | B::DeployUnverified => A::Retry,
        B::ChangeClosed => A::Cancel,
        B::BranchMoved | B::RevertConflict => A::Takeover,
        B::SecretMissing | B::SecretUnreadable => A::SetSecret,
        B::UserLinkRequired => A::LinkAccount,
        B::UserTokenInvalid => A::SetUserToken,
        B::SpecUnclear => A::EditSpec,
        B::ChangeBehindBase => A::UpdateBranch,
    }
}

pub fn primary_action(task: &Task) -> Option<PrimaryAction> {
    let blocker = task.open_blocker()?;
    let kind = match designated(blocker.code) {
        BlockerAction::UpdateBranch if !updatable(&blocker.params) => BlockerAction::Retry,
        BlockerAction::Takeover
            if blocker.code == BlockerCode::RevertConflict
                && blocker.params.get("reason").and_then(Value::as_str) == Some("empty") =>
        {
            BlockerAction::Cancel
        }
        kind => kind,
    };
    let kind = match kind {
        BlockerAction::Retry if !can_retry(task) => BlockerAction::Cancel,
        kind => kind,
    };
    let params = match kind {
        BlockerAction::SetSecret => json!({ "names": secret_names(&blocker.params) }),
        BlockerAction::LinkAccount => {
            json!({ "integration_id": blocker.params.get("integration_id").cloned().unwrap_or(Value::Null) })
        }
        _ => json!({}),
    };
    Some(PrimaryAction { kind, params })
}

fn updatable(params: &Value) -> bool {
    params.get("updatable").and_then(Value::as_bool) == Some(true)
}

fn secret_names(params: &Value) -> Vec<String> {
    let listed = params.get("names").and_then(Value::as_array);
    let named = params.get("name").and_then(Value::as_str);
    match (listed, named) {
        (Some(names), _) => names
            .iter()
            .filter_map(|n| n.as_str().map(String::from))
            .collect(),
        (None, Some(name)) => vec![name.to_string()],
        (None, None) => Vec::new(),
    }
}
