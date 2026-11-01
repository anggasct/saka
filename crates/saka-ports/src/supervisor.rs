use async_trait::async_trait;

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

#[async_trait]
pub trait Supervisor: Send + Sync {
    async fn start(&self, unit: &RunnerUnit) -> PortResult<()>;

    async fn stop(&self, name: &str) -> PortResult<()>;

    async fn is_active(&self, name: &str) -> PortResult<bool>;
}
