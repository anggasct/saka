pub mod admission;
pub mod authorship;
pub mod autonomy;
pub mod blocker;
pub mod decision;
pub mod deploy;
pub mod diagnostics;
pub mod draft;
pub mod environment;
pub mod event;
pub mod external;
pub mod gate;
pub mod hook;
pub mod identity;
pub mod ids;
pub mod inbox;
pub mod incident;
pub mod issue_intake;
pub mod issue_text;
pub mod job;
pub mod model;
pub mod paths;
pub mod plan;
pub mod policy;
pub mod queue;
pub mod react;
pub mod reconcile;
pub mod redact;
pub mod regression;
pub mod rejection;
pub mod resume;
pub mod revert;
pub mod review;
pub mod rule;
pub mod scope;
pub mod secret;
pub mod session;
pub mod shell;
pub mod spec;
pub mod task;
pub mod time;
pub mod vocab;
pub mod workflow;
pub mod workspace;

use ids::IdSource;
use time::Timestamp;

pub struct Ctx<'a> {
    pub now: Timestamp,
    pub ids: &'a mut dyn IdSource,
}
