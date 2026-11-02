pub mod admission;
pub mod event;
pub mod gate;
pub mod ids;
pub mod job;
pub mod model;
pub mod paths;
pub mod plan;
pub mod policy;
pub mod react;
pub mod reconcile;
pub mod redact;
pub mod rejection;
pub mod resume;
pub mod rule;
pub mod spec;
pub mod task;
pub mod time;
pub mod vocab;
pub mod workspace;

use ids::IdSource;
use time::Timestamp;

pub struct Ctx<'a> {
    pub now: Timestamp,
    pub ids: &'a mut dyn IdSource,
}
