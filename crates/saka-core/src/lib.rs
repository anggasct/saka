//! Pure domain core: ids, domain types, state machines, permission policy, and admission.
//! No I/O, async, clock, or randomness: the caller passes `now` and an id source.
//!
//! - `decide` turns a command or an outside fact into events, or a rejection.
//! - `evolve` applies an event to the projection rows.
//! - `react` derives the jobs (side effects) an event needs.
//! - `reconcile` derives, from the projections alone, the work that should exist.

pub mod admission;
pub mod event;
pub mod gate;
pub mod ids;
pub mod job;
pub mod model;
pub mod policy;
pub mod react;
pub mod reconcile;
pub mod rejection;
pub mod spec;
pub mod task;
pub mod time;
pub mod vocab;
pub mod workspace;

use ids::IdSource;
use time::Timestamp;

/// What every decision gets from the caller.
pub struct Ctx<'a> {
    pub now: Timestamp,
    pub ids: &'a mut dyn IdSource,
}
