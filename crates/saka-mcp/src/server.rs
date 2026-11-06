use std::sync::Arc;

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    ListToolsResult, ServerCapabilities, ServerConfig, Tool,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::transport::stdio;
use rmcp::{ErrorData, ServerHandler, ServiceExt};
use saka_core::decision::{
    CONTEXT_MAX, DETAIL_MAX, LABEL_MAX, OPTIONS_MAX, OPTIONS_MIN, QUESTION_MAX, Question,
};
use saka_ports::AskRefusal;
use serde_json::{Value, json};

use crate::SERVER_NAME;
use crate::client::{Client, ClientError};

pub const TOOL_NAME: &str = "request_decision";

const DESCRIPTION: &str = "Ask the operator to decide something you cannot settle yourself. \
Give two or three options and put the one you recommend first. The question is recorded and \
this call returns at once: end your turn after it, and the answer arrives as your next message. \
Do not use it for things you can decide from the code or the spec.";

const INSTRUCTION: &str = "Your question is recorded. End your turn now; the operator's answer \
arrives as your next message.";

const LIMIT_REACHED: &str = "limit_reached: you have asked enough questions in this run. Decide yourself and note the choice in the PR.";

fn tool_schema() -> serde_json::Map<String, Value> {
    let schema = json!({
        "type": "object",
        "properties": {
            "question": { "type": "string", "minLength": 1, "maxLength": QUESTION_MAX },
            "options": {
                "type": "array",
                "minItems": OPTIONS_MIN,
                "maxItems": OPTIONS_MAX,
                "items": {
                    "type": "object",
                    "properties": {
                        "label": { "type": "string", "minLength": 1, "maxLength": LABEL_MAX },
                        "detail": { "type": "string", "maxLength": DETAIL_MAX },
                    },
                    "required": ["label"],
                    "additionalProperties": false,
                },
            },
            "context": { "type": "string", "maxLength": CONTEXT_MAX },
        },
        "required": ["question", "options"],
        "additionalProperties": false,
    });
    match schema {
        Value::Object(map) => map,
        _ => serde_json::Map::new(),
    }
}

fn failure(text: impl Into<String>) -> CallToolResponse {
    CallToolResult::error(vec![ContentBlock::text(text.into())]).into()
}

fn refusal_text(refusal: AskRefusal) -> String {
    match refusal {
        AskRefusal::LimitReached => LIMIT_REACHED.to_string(),
        other => other.code().to_string(),
    }
}

pub struct DecisionTools {
    client: Client,
}

impl DecisionTools {
    pub fn new(client: Client) -> DecisionTools {
        DecisionTools { client }
    }

    async fn request_decision(
        &self,
        arguments: Option<serde_json::Map<String, Value>>,
    ) -> CallToolResponse {
        let Ok(question) =
            serde_json::from_value::<Question>(Value::Object(arguments.unwrap_or_default()))
        else {
            return failure(AskRefusal::InvalidInput.code());
        };
        if question.validate().is_err() {
            return failure(AskRefusal::InvalidInput.code());
        }
        match self.client.ask(question).await {
            Ok(decision_id) => CallToolResult::success(vec![ContentBlock::text(
                json!({
                    "decision_id": decision_id,
                    "status": "asked",
                    "instruction": INSTRUCTION,
                })
                .to_string(),
            )])
            .into(),
            Err(ClientError::Refused(refusal)) => failure(refusal_text(refusal)),
            Err(e) => {
                tracing::warn!(error = %e, "request_decision could not reach the engine");
                failure(AskRefusal::Unavailable.code())
            }
        }
    }
}

impl ServerHandler for DecisionTools {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(SERVER_NAME, env!("CARGO_PKG_VERSION")))
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        (name == TOOL_NAME).then(|| Tool::new(TOOL_NAME, DESCRIPTION, Arc::new(tool_schema())))
    }

    async fn list_tools(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult {
            tools: self.get_tool(TOOL_NAME).into_iter().collect(),
            ..ListToolsResult::default()
        })
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        if request.name != TOOL_NAME {
            return Err(ErrorData::invalid_params(
                format!("unknown tool {}", request.name),
                None,
            ));
        }
        Ok(self.request_decision(request.arguments).await)
    }
}

pub async fn serve_on<T, E, A>(client: Client, transport: T) -> Result<(), std::io::Error>
where
    T: rmcp::transport::IntoTransport<RoleServer, E, A>,
    E: std::error::Error + Send + Sync + 'static,
{
    let running = DecisionTools::new(client)
        .serve(transport)
        .await
        .map_err(std::io::Error::other)?;
    running.waiting().await.map_err(std::io::Error::other)?;
    Ok(())
}

pub async fn run_stdio(client: Client) -> Result<(), std::io::Error> {
    serve_on(client, stdio()).await
}
