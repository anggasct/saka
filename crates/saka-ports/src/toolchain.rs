use async_trait::async_trait;
use saka_core::model::ToolchainUse;
use saka_core::vocab::Tool;

use crate::PortResult;

#[async_trait]
pub trait Toolchains: Send + Sync {
    async fn install(&self, tool: Tool, version: &str) -> PortResult<ToolchainUse>;
}
