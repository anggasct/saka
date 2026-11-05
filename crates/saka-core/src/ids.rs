use std::fmt;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub trait IdSource {
    fn next_id(&mut self) -> Uuid;
}

macro_rules! id_type {
    ($($name:ident),* $(,)?) => {$(
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub Uuid);

        impl $name {
            pub fn new(ids: &mut dyn IdSource) -> $name {
                $name(ids.next_id())
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }
    )*};
}

id_type!(
    ProjectId,
    RepoId,
    SpecId,
    SpecRevisionId,
    WorkItemId,
    StageRunId,
    GateResultId,
    BlockerId,
    ChangeRequestId,
    EnvironmentId,
    SessionId,
    PermissionRequestId,
    PermissionRuleId,
    UsageRecordId,
    SpecDraftId,
    SpecDraftAttemptId,
    UserLinkId,
    UserLinkAuthorizationId,
    HookRunId,
);
