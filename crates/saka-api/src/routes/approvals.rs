use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use saka_core::ids::{DecisionRequestId, PermissionRequestId, PermissionRuleId, ProjectId};
use saka_engine::{AnswerKind, DecisionFilter, EngineError, PermissionFilter, Produced, RunOf};

use super::sessions::run_of;
use super::tasks::opt_id;
use super::{DECISION_STATES, PERMISSION_STATES, reply, unexpected};
use crate::dto;
use crate::http::{Mutation, body, encode_cursor, mutate, ok, page, path_id, require_key, states};
use crate::problem::ApiError;
use crate::state::AppState;

#[utoipa::path(
    get, path = "/api/v1/permission-requests", tag = "runtime", params(dto::PermissionQuery),
    responses((status = 200, body = dto::Page<dto::PermissionRequest>), (status = 422, body = dto::Problem, content_type = "application/problem+json"))
)]
pub async fn list_permissions(
    State(s): State<AppState>,
    Query(q): Query<dto::PermissionQuery>,
) -> Response {
    let engine = tri!(s.engine());
    let run = tri!(run_of(
        q.work_item_id.as_deref(),
        q.spec_draft_id.as_deref()
    ));
    let filter = PermissionFilter {
        states: tri!(states(q.state.as_deref(), PERMISSION_STATES)),
        project: tri!(opt_id(q.project_id.as_deref(), "project_id")).map(ProjectId),
        work_item: match run {
            Some(RunOf::WorkItem(id)) => Some(id),
            _ => None,
        },
        spec_draft: match run {
            Some(RunOf::SpecDraft(id)) => Some(id),
            _ => None,
        },
        source: match q.source.as_deref() {
            None => None,
            Some("policy") => Some(AnswerKind::Policy),
            Some("human") => Some(AnswerKind::Human),
            Some(_) => return ApiError::validation("source", "unknown").into_response(),
        },
    };
    let key = format!(
        "permissions:{}:{:?}:{run:?}:{:?}",
        filter.states.join(","),
        filter.project,
        filter.source,
    );
    let p = tri!(page(q.cursor.as_deref(), q.limit, &key));
    let paged = tri!(engine.permission_requests(&filter, &p));
    ok(
        StatusCode::OK,
        &dto::Page {
            items: paged
                .items
                .iter()
                .map(dto::PermissionRequest::from_view)
                .collect(),
            next_cursor: paged.next.map(|k| encode_cursor(k, &key)),
        },
    )
}

#[utoipa::path(
    post, path = "/api/v1/permission-requests/{permission_request_id}/answer", tag = "runtime",
    params(("permission_request_id" = uuid::Uuid, Path), ("Idempotency-Key" = String, Header, description = "1 to 128 printable ASCII characters")), request_body = dto::AnswerBody,
    responses((status = 200, body = dto::PermissionRequest), (status = "4XX", body = dto::Problem, content_type = "application/problem+json"))
)]
pub async fn answer_permission(
    State(s): State<AppState>,
    Path(id): Path<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    raw: Bytes,
) -> Response {
    let key = tri!(require_key(&headers));
    let request = PermissionRequestId(tri!(path_id(&id)));
    let req: dto::AnswerBody = tri!(body(&raw));
    let engine = tri!(s.engine().cloned());
    let project = tri!(engine.permission(request)).map(|v| v.project_id);
    let respond = |p: Produced<'_>| match p {
        Produced::Permission(v) => reply(StatusCode::OK, &dto::PermissionRequest::from_view(v)),
        _ => unexpected(),
    };
    let m = Mutation {
        name: "permission.answer",
        target_type: "permission_request",
        target_id: Some(request.0),
        reason: req.message.clone(),
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
                .answer_permission_with(
                    request,
                    req.decision == dto::Decision::Allow,
                    req.message,
                    req.remember,
                    &cause,
                    Some(&action),
                )
                .map(|_| project)
                .map_err(|e| (e, project))
        },
    )
    .await
}

#[utoipa::path(
    get, path = "/api/v1/decision-requests", tag = "runtime", params(dto::DecisionQuery),
    responses((status = 200, body = dto::Page<dto::DecisionRequest>), (status = 422, body = dto::Problem, content_type = "application/problem+json"))
)]
pub async fn list_decisions(
    State(s): State<AppState>,
    Query(q): Query<dto::DecisionQuery>,
) -> Response {
    let engine = tri!(s.engine());
    let run = tri!(run_of(
        q.work_item_id.as_deref(),
        q.spec_draft_id.as_deref()
    ));
    let filter = DecisionFilter {
        states: tri!(states(q.state.as_deref(), DECISION_STATES)),
        project: tri!(opt_id(q.project_id.as_deref(), "project_id")).map(ProjectId),
        work_item: match run {
            Some(RunOf::WorkItem(id)) => Some(id),
            _ => None,
        },
        spec_draft: match run {
            Some(RunOf::SpecDraft(id)) => Some(id),
            _ => None,
        },
    };
    let key = format!(
        "decisions:{}:{:?}:{run:?}",
        filter.states.join(","),
        filter.project,
    );
    let p = tri!(page(q.cursor.as_deref(), q.limit, &key));
    let paged = tri!(engine.decision_requests(&filter, &p));
    ok(
        StatusCode::OK,
        &dto::Page {
            items: paged
                .items
                .iter()
                .map(dto::DecisionRequest::from_view)
                .collect(),
            next_cursor: paged.next.map(|k| encode_cursor(k, &key)),
        },
    )
}

#[utoipa::path(
    get, path = "/api/v1/decision-requests/{decision_request_id}", tag = "runtime",
    params(("decision_request_id" = uuid::Uuid, Path)),
    responses((status = 200, body = dto::DecisionRequest), (status = 404, body = dto::Problem, content_type = "application/problem+json"))
)]
pub async fn get_decision(State(s): State<AppState>, Path(id): Path<String>) -> Response {
    let engine = tri!(s.engine());
    let id = tri!(path_id(&id));
    match tri!(engine.decision(DecisionRequestId(id))) {
        Some(v) => ok(StatusCode::OK, &dto::DecisionRequest::from_view(&v)),
        None => ApiError::from(EngineError::Rejected(
            saka_core::rejection::Rejection::DecisionNotFound,
        ))
        .into_response(),
    }
}

#[utoipa::path(
    post, path = "/api/v1/decision-requests/{decision_request_id}/answer", tag = "runtime",
    params(("decision_request_id" = uuid::Uuid, Path), ("Idempotency-Key" = String, Header, description = "1 to 128 printable ASCII characters")), request_body = dto::AnswerDecisionBody,
    responses((status = 200, body = dto::DecisionRequest), (status = "4XX", body = dto::Problem, content_type = "application/problem+json"))
)]
pub async fn answer_decision(
    State(s): State<AppState>,
    Path(id): Path<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    raw: Bytes,
) -> Response {
    let key = tri!(require_key(&headers));
    let request = DecisionRequestId(tri!(path_id(&id)));
    let req: dto::AnswerDecisionBody = tri!(body(&raw));
    let engine = tri!(s.engine().cloned());
    let project = tri!(engine.decision(request)).map(|v| v.project_id);
    let respond = |p: Produced<'_>| match p {
        Produced::Decision(v) => reply(StatusCode::OK, &dto::DecisionRequest::from_view(v)),
        _ => unexpected(),
    };
    let m = Mutation {
        name: "decision.answer",
        target_type: "decision_request",
        target_id: Some(request.0),
        reason: req.note.clone(),
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
            let option = req.option.map(|o| u32::try_from(o).unwrap_or(u32::MAX));
            engine
                .answer_decision_with(request, option, req.note, &cause, Some(&action))
                .map(|_| project)
                .map_err(|e| (e, project))
        },
    )
    .await
}

#[utoipa::path(
    get, path = "/api/v1/projects/{project_id}/permission-rules", tag = "runtime",
    params(("project_id" = uuid::Uuid, Path)),
    responses((status = 200, body = dto::PermissionRuleList), (status = 404, body = dto::Problem, content_type = "application/problem+json"))
)]
pub async fn list_permission_rules(State(s): State<AppState>, Path(id): Path<String>) -> Response {
    let engine = tri!(s.engine());
    let id = ProjectId(tri!(path_id(&id)));
    match tri!(engine.permission_rules(id)) {
        Some(rules) => ok(
            StatusCode::OK,
            &dto::PermissionRuleList {
                items: rules.iter().map(dto::PermissionRule::from).collect(),
                operator_rules: engine
                    .operator_rules()
                    .iter()
                    .map(dto::OperatorPermissionRule::from)
                    .collect(),
            },
        ),
        None => ApiError::not_found().into_response(),
    }
}

#[utoipa::path(
    post, path = "/api/v1/projects/{project_id}/permission-rules", tag = "runtime",
    params(("project_id" = uuid::Uuid, Path), ("Idempotency-Key" = String, Header, description = "1 to 128 printable ASCII characters")), request_body = dto::NewPermissionRule,
    responses((status = 201, body = dto::PermissionRule), (status = "4XX", body = dto::Problem, content_type = "application/problem+json"))
)]
pub async fn add_permission_rule(
    State(s): State<AppState>,
    Path(id): Path<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    raw: Bytes,
) -> Response {
    let key = tri!(require_key(&headers));
    let project = ProjectId(tri!(path_id(&id)));
    let req: dto::NewPermissionRule = tri!(body(&raw));
    let engine = tri!(s.engine().cloned());
    let respond = |p: Produced<'_>| match p {
        Produced::Rule(r) => reply(StatusCode::CREATED, &dto::PermissionRule::from(r)),
        _ => unexpected(),
    };
    let m = Mutation {
        name: "permission_rule.add",
        target_type: "project",
        target_id: Some(project.0),
        reason: req.note.clone(),
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
                .add_rule(project, req.into(), &cause, Some(&action))
                .map(|_| Some(project))
                .map_err(|e| (e, Some(project)))
        },
    )
    .await
}

#[utoipa::path(
    post, path = "/api/v1/projects/{project_id}/permission-rules/preview", tag = "runtime",
    params(("project_id" = uuid::Uuid, Path)), request_body = dto::NewPermissionRule,
    responses((status = 200, body = dto::PermissionPreview), (status = "4XX", body = dto::Problem, content_type = "application/problem+json"))
)]
pub async fn preview_permission_rule(
    State(s): State<AppState>,
    Path(id): Path<String>,
    raw: Bytes,
) -> Response {
    let project = ProjectId(tri!(path_id(&id)));
    let req: dto::NewPermissionRule = tri!(body(&raw));
    let engine = tri!(s.engine());
    let preview = tri!(engine.preview_rule(project, req.into()));
    ok(StatusCode::OK, &dto::PermissionPreview::from(&preview))
}

#[utoipa::path(
    post, path = "/api/v1/projects/{project_id}/permission-rules/evaluate", tag = "runtime",
    params(("project_id" = uuid::Uuid, Path)), request_body = dto::EvaluateBody,
    responses((status = 200, body = dto::PermissionEvaluation), (status = "4XX", body = dto::Problem, content_type = "application/problem+json"))
)]
pub async fn evaluate_permission(
    State(s): State<AppState>,
    Path(id): Path<String>,
    raw: Bytes,
) -> Response {
    let project = ProjectId(tri!(path_id(&id)));
    let req: dto::EvaluateBody = tri!(body(&raw));
    if req.tool.trim().is_empty() {
        return ApiError::validation("tool", "required").into_response();
    }
    let engine = tri!(s.engine());
    let request = saka_core::policy::ToolRequest {
        tool: req.tool,
        command: req.command,
        path: req.path.map(|path| saka_core::policy::RequestPath {
            path,
            inside_worktree: true,
            through_symlink: false,
            in_reference: false,
        }),
    };
    let decision = tri!(engine.evaluate_permission(project, &request));
    ok(StatusCode::OK, &dto::PermissionEvaluation::from(&decision))
}

#[utoipa::path(
    post, path = "/api/v1/permission-rules/{rule_id}/revoke", tag = "runtime",
    params(("rule_id" = uuid::Uuid, Path), ("Idempotency-Key" = String, Header, description = "1 to 128 printable ASCII characters")), request_body = dto::RequiredReason,
    responses((status = 200, body = dto::PermissionRule), (status = "4XX", body = dto::Problem, content_type = "application/problem+json"))
)]
pub async fn revoke_rule(
    State(s): State<AppState>,
    Path(id): Path<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    raw: Bytes,
) -> Response {
    let key = tri!(require_key(&headers));
    let rule = PermissionRuleId(tri!(path_id(&id)));
    let req: dto::RequiredReason = tri!(body(&raw));
    let engine = tri!(s.engine().cloned());
    let respond = |p: Produced<'_>| match p {
        Produced::Rule(r) => reply(StatusCode::OK, &dto::PermissionRule::from(r)),
        _ => unexpected(),
    };
    let m = Mutation {
        name: "permission_rule.revoke",
        target_type: "permission_rule",
        target_id: Some(rule.0),
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
                .revoke_rule(rule, &req.reason, &cause, Some(&action))
                .map(|r| Some(r.project_id))
                .map_err(|e| (e, None))
        },
    )
    .await
}
