use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::Response;
use saka_engine::Produced;

use super::{no_content, reply, unexpected};
use crate::dto;
use crate::http::{Mutation, body, mutate, ok, path_id, require_key};
use crate::state::AppState;

#[utoipa::path(
    post, path = "/api/v1/scm/{integration_id}/user-link/authorizations", tag = "accounts",
    params(("integration_id" = String, Path, description = "`[[scm]].id` from the operator config"), ("Idempotency-Key" = String, Header, description = "1 to 128 printable ASCII characters")),
    request_body = dto::StartUserLinkBody,
    responses(
        (status = 202, body = dto::UserLinkAuthorization, description = "A pending authorization: a new one, or the one still pending with more than a minute left"),
        (status = "4XX", body = dto::Problem, content_type = "application/problem+json"),
        (status = 502, body = dto::Problem, content_type = "application/problem+json"),
    )
)]
pub async fn start_user_link(
    State(s): State<AppState>,
    Path(integration): Path<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    raw: Bytes,
) -> Response {
    let key = tri!(require_key(&headers));
    let _: dto::StartUserLinkBody = tri!(body(&raw));
    let engine = tri!(s.engine().cloned());
    let respond = |p: Produced<'_>| match p {
        Produced::UserLinkAuthorization(v) => reply(
            StatusCode::ACCEPTED,
            &dto::UserLinkAuthorization::from_view(v),
        ),
        _ => unexpected(),
    };
    let m = Mutation {
        name: "user_link.authorize",
        target_type: "user_link_authorization",
        target_id: None,
        reason: None,
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
                .start_user_link(&integration, &cause, Some(&action))
                .await
                .map(|_| None)
                .map_err(|e| (e, None))
        },
    )
    .await
}

#[utoipa::path(
    get, path = "/api/v1/scm/{integration_id}/user-link/authorizations/{authorization_id}", tag = "accounts",
    params(("integration_id" = String, Path, description = "`[[scm]].id` from the operator config"), ("authorization_id" = uuid::Uuid, Path)),
    responses(
        (status = 200, body = dto::UserLinkAuthorization),
        (status = 404, body = dto::Problem, content_type = "application/problem+json"),
    )
)]
pub async fn get_user_link_authorization(
    State(s): State<AppState>,
    Path((integration, id)): Path<(String, String)>,
) -> Response {
    let engine = tri!(s.engine());
    let id = saka_core::ids::UserLinkAuthorizationId(tri!(path_id(&id)));
    let view = tri!(engine.user_link_authorization(&integration, saka_engine::OPERATOR, id));
    ok(
        StatusCode::OK,
        &dto::UserLinkAuthorization::from_view(&view),
    )
}

#[utoipa::path(
    get, path = "/api/v1/scm/{integration_id}/user-link", tag = "accounts",
    params(("integration_id" = String, Path, description = "`[[scm]].id` from the operator config")),
    responses(
        (status = 200, body = dto::UserLink),
        (status = 404, body = dto::Problem, content_type = "application/problem+json", description = "`not_found` for an unknown integration, `user_link_not_found` without a linked account"),
    )
)]
pub async fn get_user_link(State(s): State<AppState>, Path(integration): Path<String>) -> Response {
    let engine = tri!(s.engine());
    let link = tri!(engine.user_link(&integration, saka_engine::OPERATOR));
    ok(StatusCode::OK, &dto::UserLink::from(&link))
}

#[utoipa::path(
    delete, path = "/api/v1/scm/{integration_id}/user-link", tag = "accounts",
    params(("integration_id" = String, Path, description = "`[[scm]].id` from the operator config"), ("Idempotency-Key" = String, Header, description = "1 to 128 printable ASCII characters")),
    responses(
        (status = 204, description = "The link is gone, or there was none; the tokens are not revoked at the provider"),
        (status = "4XX", body = dto::Problem, content_type = "application/problem+json"),
    )
)]
pub async fn remove_user_link(
    State(s): State<AppState>,
    Path(integration): Path<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    raw: Bytes,
) -> Response {
    let key = tri!(require_key(&headers));
    let engine = tri!(s.engine().cloned());
    let m = Mutation {
        name: "user_link.remove",
        target_type: "user_link",
        target_id: None,
        reason: None,
    };
    mutate(
        &s,
        key,
        &method,
        uri.path(),
        &raw,
        m,
        &no_content,
        |cause, action| async move {
            engine
                .remove_user_link(&integration, &cause, Some(&action))
                .map(|()| None)
                .map_err(|e| (e, None))
        },
    )
    .await
}
