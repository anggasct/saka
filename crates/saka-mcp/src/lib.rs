pub mod client;
pub mod listener;
pub mod server;
pub mod wire;

pub use client::{Client, ClientError};
pub use listener::{Tokens, bind, serve};
pub use server::{TOOL_NAME, run_stdio, serve_on};

pub const SOCKET_ENV: &str = "SAKA_MCP_SOCKET";
pub const TOKEN_ENV: &str = "SAKA_MCP_TOKEN";
pub const SERVER_NAME: &str = "saka";
pub const TOOL_FULL_NAME: &str = saka_core::policy::DECISION_TOOL;
