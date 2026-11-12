use std::path::PathBuf;
use std::process::ExitCode;

use saka_core::ids::SessionId;
use saka_mcp::{Client, ClientError, SOCKET_ENV, TOKEN_ENV};
use serde_json::json;
use uuid::Uuid;

use crate::cli;
use crate::serve::EXIT_REFUSED;

pub async fn run(session: &str, regression_check: bool) -> ExitCode {
    let Ok(session) = Uuid::parse_str(session).map(SessionId) else {
        cli::refuse("mcp_session_invalid", &json!({}));
        return ExitCode::from(EXIT_REFUSED);
    };
    let (Some(socket), Some(token)) = (std::env::var_os(SOCKET_ENV), std::env::var(TOKEN_ENV).ok())
    else {
        cli::refuse("mcp_endpoint_missing", &json!({}));
        return ExitCode::from(EXIT_REFUSED);
    };
    let client = Client::new(PathBuf::from(socket), session, token);
    match client.hello().await {
        Ok(()) => {}
        Err(ClientError::Unauthorized) => {
            cli::refuse("mcp_token_refused", &json!({}));
            return ExitCode::from(EXIT_REFUSED);
        }
        Err(e) => {
            cli::refuse("mcp_socket_unreachable", &json!({ "error": e.to_string() }));
            return ExitCode::from(EXIT_REFUSED);
        }
    }
    match saka_mcp::run_stdio(client, regression_check).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            cli::refuse("mcp_failed", &json!({ "error": e.to_string() }));
            ExitCode::FAILURE
        }
    }
}
