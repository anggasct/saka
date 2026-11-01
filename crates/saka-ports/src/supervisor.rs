//! Runs each session's runner outside the server's own process tree, with resource
//! limits, so the server can restart without killing sessions and a session cannot
//! starve the host.

use async_trait::async_trait;

use crate::PortResult;

/// Resource limits for one runner and everything it spawns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnitLimits {
    /// systemd `MemoryMax`, e.g. `4G`.
    pub memory_max: String,
    /// systemd `CPUQuota`, e.g. `200%`.
    pub cpu_quota: String,
    /// systemd `TasksMax`.
    pub tasks_max: u32,
}

/// A runner to start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunnerUnit {
    /// `saka-session-<session id>`.
    pub name: String,
    pub program: String,
    pub args: Vec<String>,
    pub limits: UnitLimits,
}

#[async_trait]
pub trait Supervisor: Send + Sync {
    /// Starts the unit. A unit with the same name that is still running is stopped first.
    async fn start(&self, unit: &RunnerUnit) -> PortResult<()>;

    /// Stops the unit and every process in it. A unit that is gone is a success.
    async fn stop(&self, name: &str) -> PortResult<()>;
}
