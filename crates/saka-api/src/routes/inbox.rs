use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use saka_core::ids::ProjectId;
use saka_engine::InboxFilter;

use super::tasks::opt_id;
use crate::dto;
use crate::http::{encode_cursor, page, tagged};
use crate::problem::ApiError;
use crate::state::AppState;

#[utoipa::path(
    get, path = "/api/v1/inbox", tag = "inbox", params(dto::InboxQuery),
    responses(
        (status = 200, body = dto::Page<dto::InboxItem>, headers(("ETag" = String, description = "A digest of the page; send it back as If-None-Match"))),
        (status = 304, description = "The page is unchanged"),
        (status = 422, body = dto::Problem, content_type = "application/problem+json"),
    )
)]
pub async fn list_inbox(
    State(s): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<dto::InboxQuery>,
) -> Response {
    let engine = tri!(s.engine());
    let kind = match q.kind.as_deref().filter(|v| !v.is_empty()) {
        None => None,
        Some(v) => Some(tri!(
            v.parse()
                .map_err(|_| ApiError::validation("kind", "invalid"))
        )),
    };
    let closed = match q.state.as_deref().filter(|v| !v.is_empty()) {
        None | Some("open") => false,
        Some("closed") => true,
        Some(_) => return ApiError::validation("state", "invalid").into_response(),
    };
    let filter = InboxFilter {
        project: tri!(opt_id(q.project_id.as_deref(), "project_id")).map(ProjectId),
        kind,
        closed,
    };
    let key = format!("inbox:{:?}:{:?}:{}", filter.project, filter.kind, closed);
    let p = tri!(page(q.cursor.as_deref(), q.limit, &key));
    let paged = tri!(engine.inbox(&filter, &p));
    tagged(
        &headers,
        &dto::Page {
            items: paged.items.iter().map(dto::InboxItem::from).collect(),
            next_cursor: paged.next.map(|k| encode_cursor(k, &key)),
        },
    )
}

#[utoipa::path(
    get, path = "/api/v1/inbox/summary", tag = "inbox",
    responses(
        (status = 200, body = dto::InboxSummary, headers(("ETag" = String, description = "A digest of the summary; send it back as If-None-Match"))),
        (status = 304, description = "The summary is unchanged"),
    )
)]
pub async fn inbox_summary(State(s): State<AppState>, headers: HeaderMap) -> Response {
    let engine = tri!(s.engine());
    let summary = tri!(engine.inbox_summary());
    tagged(&headers, &dto::InboxSummary::from(&summary))
}
