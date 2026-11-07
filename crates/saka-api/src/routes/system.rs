use std::collections::VecDeque;
use std::convert::Infallible;

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use futures_util::stream;
use saka_core::ids::SessionId;
use saka_engine::StoredEvent;

use super::sessions::run_of;
use super::tasks::opt_id;
use super::{EVENT_BATCH, EVENT_POLL};
use crate::dto::{self, CAPABILITIES};
use crate::http::{encode_cursor, ok, page};
use crate::problem::ApiError;
use crate::state::AppState;

#[utoipa::path(
    get, path = "/api/v1/agent-accounts", tag = "runtime",
    responses((status = 200, body = dto::AgentAccountList))
)]
pub async fn list_agent_accounts(State(s): State<AppState>) -> Response {
    let engine = tri!(s.engine());
    let accounts = tri!(engine.agent_accounts());
    ok(
        StatusCode::OK,
        &dto::AgentAccountList {
            items: accounts.iter().map(dto::AgentAccount::from).collect(),
        },
    )
}

#[utoipa::path(
    get, path = "/api/v1/usage-records", tag = "runtime", params(dto::UsageQuery),
    responses((status = 200, body = dto::Page<dto::UsageRecord>), (status = 422, body = dto::Problem, content_type = "application/problem+json"))
)]
pub async fn list_usage_records(
    State(s): State<AppState>,
    Query(q): Query<dto::UsageQuery>,
) -> Response {
    let engine = tri!(s.engine());
    let run = tri!(run_of(
        q.work_item_id.as_deref(),
        q.spec_draft_id.as_deref()
    ));
    let session = tri!(opt_id(q.session_id.as_deref(), "session_id")).map(SessionId);
    if run.is_none() && session.is_none() {
        return ApiError::validation("work_item_id", "required").into_response();
    }
    let key = format!("usage:{run:?}:{session:?}");
    let p = tri!(page(q.cursor.as_deref(), q.limit, &key));
    let rows = tri!(engine.usage_records(run, session));
    let mut items: Vec<_> = rows
        .iter()
        .filter(|u| {
            p.after
                .is_none_or(|a| (u.record.ended_at.0, u.record.id.0) > a)
        })
        .collect();
    let limit = usize::try_from(p.limit).unwrap_or(usize::MAX);
    let more = items.len() > limit;
    items.truncate(limit);
    ok(
        StatusCode::OK,
        &dto::Page {
            next_cursor: if more {
                items
                    .last()
                    .map(|u| encode_cursor((u.record.ended_at.0, u.record.id.0), &key))
            } else {
                None
            },
            items: items.into_iter().map(dto::UsageRecord::from).collect(),
        },
    )
}

#[utoipa::path(
    get, path = "/api/v1/meta", tag = "system",
    responses((status = 200, body = dto::Meta))
)]
pub async fn meta(State(s): State<AppState>) -> Response {
    ok(
        StatusCode::OK,
        &dto::Meta {
            server_version: s.config().server_version.clone(),
            api_version: dto::API_VERSION.into(),
            min_client_api_version: dto::API_VERSION.into(),
            capabilities: CAPABILITIES.iter().map(|c| c.to_string()).collect(),
        },
    )
}

pub(super) fn sse_event(e: &StoredEvent) -> Event {
    let data = dto::StreamEvent {
        seq: e.seq,
        kind: e.kind.clone(),
        version: e.version,
        stream_type: e.stream_type.clone(),
        stream_id: e.stream_id,
        project_id: e.project_id.map(|p| p.0),
        occurred_at: dto::time(e.occurred_at),
        payload: e.payload.clone(),
    };
    Event::default()
        .id(e.seq.to_string())
        .event(format!("{}.v{}", e.kind, e.version))
        .data(serde_json::to_string(&data).unwrap_or_default())
}

pub(super) fn activity_event(a: &saka_core::model::SessionActivity) -> Event {
    let data = dto::ActivityNotice {
        session_id: a.session_id.0,
        owner: dto::RunOwner::from(&a.owner),
        offset: a.offset,
        kind: a.kind.clone(),
    };
    Event::default()
        .event("runtime.session.activity.v1")
        .data(serde_json::to_string(&data).unwrap_or_default())
}

#[utoipa::path(
    get, path = "/api/v1/events", tag = "system",
    params(dto::EventsQuery, ("Last-Event-ID" = Option<String>, Header, description = "Resume after this seq")),
    responses(
        (status = 200, content_type = "text/event-stream", body = dto::StreamEvent, description = "One `data` per event, `id` = seq. `runtime.session.activity.v1` messages carry an ActivityNotice and no `id`."),
        (status = 422, body = dto::Problem, content_type = "application/problem+json"),
    )
)]
pub async fn events(
    State(s): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<dto::EventsQuery>,
) -> Response {
    let engine = tri!(s.engine().cloned());
    let from_header = headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .map(String::from);
    let after = match from_header.or(q.after) {
        Some(raw) => tri!(
            raw.trim()
                .parse::<i64>()
                .map_err(|_| ApiError::validation("after", "not_a_number"))
        ),
        None => tri!(engine.last_seq()),
    };
    let activity_cursor = tri!(engine.last_activity_seq());
    let stream = stream::unfold(
        (engine, (after, activity_cursor), VecDeque::<Event>::new()),
        |(engine, (mut cursor, mut activity_cursor), mut buffer)| async move {
            loop {
                if let Some(event) = buffer.pop_front() {
                    return Some((
                        Ok::<Event, Infallible>(event),
                        (engine, (cursor, activity_cursor), buffer),
                    ));
                }
                match engine.events_after(cursor, EVENT_BATCH) {
                    Ok(list) if !list.is_empty() => {
                        cursor = list.last().map_or(cursor, |e| e.seq);
                        buffer.extend(list.iter().map(sse_event));
                    }
                    Ok(_) => {}
                    Err(e) => tracing::warn!(error = %e, "reading events failed"),
                }
                match engine.activities_after(activity_cursor, EVENT_BATCH) {
                    Ok(list) if !list.is_empty() => {
                        activity_cursor = list.last().map_or(activity_cursor, |(seq, _)| *seq);
                        buffer.extend(list.iter().map(|(_, a)| activity_event(a)));
                    }
                    Ok(_) => {}
                    Err(e) => tracing::warn!(error = %e, "reading session activity failed"),
                }
                if !buffer.is_empty() {
                    continue;
                }
                tokio::select! {
                    () = engine.changed() => {}
                    () = tokio::time::sleep(EVENT_POLL) => {}
                }
            }
        },
    );
    Sse::new(stream)
        .keep_alive(
            KeepAlive::new()
                .interval(s.config().keep_alive)
                .text("keep-alive"),
        )
        .into_response()
}

pub async fn api_not_found() -> Response {
    ApiError::not_found().into_response()
}
