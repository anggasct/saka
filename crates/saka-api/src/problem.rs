use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use saka_core::rejection::Rejection;
use saka_engine::EngineError;
use serde_json::{Value, json};
use uuid::Uuid;

tokio::task_local! {
    pub static CORRELATION: Uuid;
}

pub fn correlation() -> Option<Uuid> {
    CORRELATION.try_with(|c| *c).ok()
}

pub const PROBLEM_JSON: &str = "application/problem+json";

#[derive(Debug, Clone, PartialEq)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: String,
    pub params: Value,
}

impl ApiError {
    pub fn new(status: StatusCode, code: &str, params: Value) -> ApiError {
        ApiError {
            status,
            code: code.to_string(),
            params,
        }
    }

    pub fn not_found() -> ApiError {
        ApiError::new(StatusCode::NOT_FOUND, "not_found", json!({}))
    }

    pub fn validation(field: &str, code: &str) -> ApiError {
        ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "validation_failed",
            json!({ "errors": [{ "field": field, "code": code }] }),
        )
    }

    pub fn internal() -> ApiError {
        ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", json!({}))
    }

    pub fn body(&self) -> Value {
        json!({
            "type": format!("urn:saka:problem:{}", self.code),
            "title": self.code,
            "status": self.status.as_u16(),
            "code": self.code,
            "params": self.params,
        })
    }
}

pub fn problem_response(status: StatusCode, mut body: Value) -> Response {
    let id = correlation();
    if let Value::Object(map) = &mut body {
        map.insert("correlation_id".into(), json!(id));
        if status == StatusCode::INTERNAL_SERVER_ERROR
            && let Some(Value::Object(params)) = map.get_mut("params")
        {
            params.insert("correlation_id".into(), json!(id));
        }
    }
    let mut response = (status, body.to_string()).into_response();
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(PROBLEM_JSON));
    response
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        problem_response(self.status, self.body())
    }
}

fn rejection_status(r: &Rejection) -> StatusCode {
    use Rejection as R;
    match r {
        R::NotFound | R::SpecFileNotFound { .. } | R::UserLinkNotFound => StatusCode::NOT_FOUND,
        R::RevisionStale | R::VersionStale | R::FileStale { .. } => StatusCode::PRECONDITION_FAILED,
        R::SpecNotReady { .. }
        | R::ScmIntegrationUnknown { .. }
        | R::ProjectConfigInvalid { .. }
        | R::ScmRepoInaccessible { .. }
        | R::SecretTooShort
        | R::SecretNameInvalid
        | R::FileUnsupported { .. }
        | R::Invalid { .. } => StatusCode::UNPROCESSABLE_ENTITY,
        R::InvalidState { .. }
        | R::Unsupported
        | R::SpecAlreadyApproved
        | R::SpecReparseRequired { .. }
        | R::RevisionConflict { .. }
        | R::SpecHasActiveTasks { .. }
        | R::MergeNotReady { .. }
        | R::PermissionAlreadyAnswered
        | R::ProjectExists { .. }
        | R::ProjectRepoInUse { .. }
        | R::SpecSlugConflict { .. }
        | R::TaskBranchConflict { .. }
        | R::WorkPacketUnavailable { .. }
        | R::FilePathConflict { .. }
        | R::FilePushRejected { .. }
        | R::RetryLimitReached { .. }
        | R::SpecDraftTargetTaken { .. }
        | R::DeviceFlowUnconfigured { .. }
        | R::UserAuthorshipUnsupported { .. }
        | R::UserLinkInUse { .. } => StatusCode::CONFLICT,
    }
}

impl From<EngineError> for ApiError {
    fn from(e: EngineError) -> ApiError {
        match e {
            EngineError::Rejected(r) => ApiError::new(rejection_status(&r), r.code(), r.params()),
            EngineError::Invalid { field, code } => ApiError::validation(field, &code),
            EngineError::InvalidItem { field, code } => ApiError::validation(&field, &code),
            EngineError::Port(p) if p.code == "scm_repo_inaccessible" => ApiError::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "scm_repo_inaccessible",
                p.params,
            ),
            EngineError::Port(p) => {
                tracing::warn!(code = %p.code, "adapter call failed");
                ApiError::new(
                    StatusCode::BAD_GATEWAY,
                    "scm_unavailable",
                    json!({ "provider_status": p.params.get("status").cloned().unwrap_or(Value::Null) }),
                )
            }
            EngineError::Store(e) => {
                tracing::error!(error = %e, code = e.code(), "store failed");
                ApiError::internal()
            }
            EngineError::Internal(e) => {
                tracing::error!(error = %e, "internal error");
                ApiError::internal()
            }
        }
    }
}
