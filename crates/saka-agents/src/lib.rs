//! Agent CLI adapters and the runner protocol.
//!
//! - [`runner`]: the `saka runner` process (one per session).
//! - [`protocol`]: frames between the server and a runner (v1).
//! - [`client`] and [`adapter`]: the server side, behind the `Agents` port.
//! - [`claude`] and [`launch`]: everything specific to Claude Code.
//! - [`redact`]: applied to all CLI output before it is stored or sent.

pub mod adapter;
pub mod claude;
pub mod client;
pub mod launch;
pub mod protocol;
pub mod redact;
pub mod runner;

pub use adapter::{AgentsConfig, ClaudeAgents, unit_name};
