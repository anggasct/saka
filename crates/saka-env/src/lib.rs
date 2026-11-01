//! Task environments (git worktrees) and the session supervisor.

pub mod supervisor;

pub use supervisor::{DirectSupervisor, SystemdSupervisor};
