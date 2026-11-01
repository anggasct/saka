//! Port traits and data types shared by the engine and the adapters.
//!
//! Adapter ports (agent CLI, supervisor, environment, source control, docs) are added
//! with their first adapter; time and ids are needed by every caller of the core.

use saka_core::ids::IdSource;
use saka_core::time::Timestamp;
use uuid::Uuid;

/// Wall-clock time in milliseconds since the Unix epoch (UTC). Virtual in tests.
pub trait Clock: Send + Sync {
    fn now(&self) -> Timestamp;
}

/// New ids: UUIDv7 in production, deterministic in tests.
pub trait Ids: Send + Sync {
    fn new_id(&self) -> Uuid;
}

/// Lends an [`Ids`] port to the core, which takes ids through [`IdSource`].
pub struct IdsSource<'a>(pub &'a dyn Ids);

impl IdSource for IdsSource<'_> {
    fn next_id(&mut self) -> Uuid {
        self.0.new_id()
    }
}
