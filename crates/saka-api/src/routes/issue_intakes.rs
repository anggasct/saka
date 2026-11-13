use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use saka_core::ids::{IssueIntakeId, ProjectId};

use crate::dto;
use crate::http::{encode_cursor, ok, page, path_id, states};
use crate::state::AppState;

const INTAKE_STATES: &[&str] = &[
    "accepted",
    "drafting",
    "awaiting_approval",
    "approved",
    "in_progress",
    "done",
    "dismissed",
    "closed",
    "failed",
];

#[utoipa::path(
    get, path = "/api/v1/projects/{project_id}/issue-intakes", tag = "issue-intakes",
    params(("project_id" = uuid::Uuid, Path), dto::IssueIntakeQuery),
    responses(
        (status = 200, body = dto::Page<dto::IssueIntake>),
        (status = "4XX", body = dto::Problem, content_type = "application/problem+json"),
    )
)]
pub async fn list_issue_intakes(
    State(s): State<AppState>,
    Path(project): Path<String>,
    Query(q): Query<dto::IssueIntakeQuery>,
) -> Response {
    let engine = tri!(s.engine());
    let project = ProjectId(tri!(path_id(&project)));
    let wanted = tri!(states(q.state.as_deref(), INTAKE_STATES));
    let key = format!("issue_intakes:{project}:{}", wanted.join(","));
    let p = tri!(page(q.cursor.as_deref(), q.limit, &key));
    let paged = tri!(engine.issue_intakes(project, &wanted, &p));
    ok(
        StatusCode::OK,
        &dto::Page {
            items: paged
                .items
                .iter()
                .map(dto::IssueIntake::from_view)
                .collect(),
            next_cursor: paged.next.map(|k| encode_cursor(k, &key)),
        },
    )
}

#[utoipa::path(
    get, path = "/api/v1/issue-intakes/{intake_id}", tag = "issue-intakes",
    params(("intake_id" = uuid::Uuid, Path)),
    responses(
        (status = 200, body = dto::IssueIntake),
        (status = 404, body = dto::Problem, content_type = "application/problem+json"),
    )
)]
pub async fn get_issue_intake(State(s): State<AppState>, Path(id): Path<String>) -> Response {
    let engine = tri!(s.engine());
    let id = IssueIntakeId(tri!(path_id(&id)));
    let view = tri!(engine.issue_intake(id));
    ok(StatusCode::OK, &dto::IssueIntake::from_view(&view))
}
