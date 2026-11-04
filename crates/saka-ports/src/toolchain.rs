use async_trait::async_trait;
use saka_core::diagnostics::ToolchainCache;
use saka_core::model::ToolchainUse;
use saka_core::vocab::{HostTool, Tool};

use crate::PortResult;

#[async_trait]
pub trait Toolchains: Send + Sync {
    async fn install(&self, tool: Tool, version: &str) -> PortResult<ToolchainUse>;

    async fn cache(&self) -> PortResult<ToolchainCache>;

    async fn host_version(&self, tool: HostTool) -> PortResult<Option<String>>;
}
