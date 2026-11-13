macro_rules! tri {
    ($e:expr) => {
        match $e {
            Ok(v) => v,
            Err(e) => {
                return axum::response::IntoResponse::into_response(
                    $crate::problem::ApiError::from(e),
                );
            }
        }
    };
}

pub mod approvals;
pub mod deployments;
pub mod drafts;
pub mod inbox;
pub mod incidents;
pub mod issue_intakes;
pub mod operations;
pub mod projects;
pub mod sessions;
pub mod specs;
pub mod system;
pub mod tasks;
pub mod user_links;

use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::Response;
use saka_core::ids::InboxItemId;
use saka_engine::{Produced, Reply};
use serde_json::Value;

use crate::dto::{self};
use crate::http::{Mutation, body, mutate, path_id, require_key};
use crate::problem::ApiError;
use crate::state::AppState;

const SPEC_STATES: &[&str] = &["draft", "ready", "approved", "shipped", "withdrawn"];
const WORK_ITEM_STATES: &[&str] = &[
    "queued",
    "awaiting_start",
    "admitted",
    "in_stage",
    "blocked",
    "taken_over",
    "done",
    "cancelled",
    "superseded",
];
const PERMISSION_STATES: &[&str] = &["pending", "allowed", "denied", "expired"];
const DECISION_STATES: &[&str] = &["asked", "answered", "delivered", "expired"];
const EVENT_BATCH: u32 = 100;
const EVENT_POLL: Duration = Duration::from_millis(500);

fn reply<T: serde::Serialize>(status: StatusCode, value: &T) -> Reply {
    Reply {
        status: status.as_u16(),
        body: serde_json::to_value(value).unwrap_or(Value::Null),
    }
}

fn unexpected() -> Reply {
    Reply {
        status: 500,
        body: ApiError::internal().body(),
    }
}

fn no_content(p: Produced<'_>) -> Reply {
    match p {
        Produced::Nothing => Reply {
            status: StatusCode::NO_CONTENT.as_u16(),
            body: Value::Null,
        },
        _ => unexpected(),
    }
}

#[utoipa::path(
    post, path = "/api/v1/inbox/{inbox_item_id}/dismiss", tag = "inbox",
    params(("inbox_item_id" = uuid::Uuid, Path), ("Idempotency-Key" = String, Header, description = "1 to 128 printable ASCII characters")),
    request_body = dto::RequiredReason,
    responses((status = 200, body = dto::InboxItem), (status = "4XX", body = dto::Problem, content_type = "application/problem+json"))
)]
pub async fn dismiss_inbox_item(
    State(s): State<AppState>,
    Path(id): Path<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    raw: Bytes,
) -> Response {
    let key = tri!(require_key(&headers));
    let item = InboxItemId(tri!(path_id(&id)));
    let req: dto::RequiredReason = tri!(body(&raw));
    let engine = tri!(s.engine().cloned());
    let project = tri!(engine.inbox_item(item)).map(|i| i.project_id);
    let respond = |p: Produced<'_>| match p {
        Produced::Inbox(i) => reply(StatusCode::OK, &dto::InboxItem::from(i)),
        _ => unexpected(),
    };
    let m = Mutation {
        name: "inbox_item.dismiss",
        target_type: "inbox_item",
        target_id: Some(item.0),
        reason: Some(req.reason.clone()),
    };
    mutate(
        &s,
        key,
        &method,
        uri.path(),
        &raw,
        m,
        &respond,
        |cause, action| async move {
            engine
                .dismiss_inbox_item_with(item, &req.reason, &cause, Some(&action))
                .map(|i| Some(i.project_id))
                .map_err(|e| (e, project))
        },
    )
    .await
}
