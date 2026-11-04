use std::collections::BTreeMap;
use std::time::Duration;

use async_trait::async_trait;
use saka_core::diagnostics::SupervisorProbe;
use saka_core::secret::SecretText;

use crate::PortResult;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnitLimits {
    pub memory_max: String,
    pub cpu_quota: String,
    pub tasks_max: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunnerUnit {
    pub name: String,
    pub program: String,
    pub args: Vec<String>,
    pub limits: UnitLimits,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandUnit {
    pub name: String,
    pub program: String,
    pub args: Vec<String>,
    pub cwd: String,
    pub env: BTreeMap<String, String>,
    pub secret_env: Vec<(String, SecretText)>,
    pub limits: UnitLimits,
    pub timeout: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutcome {
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub output: Vec<u8>,
    pub truncated: bool,
}

pub const COMMAND_OUTPUT_MAX: usize = 8 * 1024 * 1024;

#[async_trait]
pub trait Supervisor: Send + Sync {
    async fn start(&self, unit: &RunnerUnit) -> PortResult<()>;

    async fn stop(&self, name: &str) -> PortResult<()>;

    async fn is_active(&self, name: &str) -> PortResult<bool>;

    async fn run(&self, unit: &CommandUnit) -> PortResult<CommandOutcome>;

    async fn probe(&self) -> PortResult<SupervisorProbe>;
}
