use axum::Extension;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use saka_core::ids::DeviceId;
use saka_core::vocab::{DeviceKind, PairingPurpose};
use saka_engine::{Produced, Reply};
use serde_json::{Map, Value, json};

use crate::auth::Caller;
use crate::dto;
use crate::http::{Mutation, Reveal, body, mutate, mutate_revealing, ok, path_id, require_key};
use crate::problem::{ApiError, correlation};
use crate::state::AppState;

macro_rules! tri {
    ($e:expr) => {
        match $e {
            Ok(v) => v,
            Err(e) => return ApiError::from(e).into_response(),
        }
    };
}

fn disabled() -> Response {
    ApiError::new(StatusCode::NOT_FOUND, "auth_disabled", json!({})).into_response()
}

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

#[utoipa::path(
    get, path = "/api/v1/auth/status", tag = "auth",
    responses((status = 200, body = dto::AuthStatus), (status = 404, body = dto::Problem, content_type = "application/problem+json"))
)]
pub async fn status(State(s): State<AppState>, Extension(caller): Extension<Caller>) -> Response {
    if s.authenticator().is_none() {
        return disabled();
    }
    let engine = tri!(s.engine());
    ok(
        StatusCode::OK,
        &dto::AuthStatus {
            enabled: true,
            setup_required: tri!(engine.setup_required()),
            authenticated: caller.0.is_some(),
            actor: caller.0.map(|p| p.actor.to_string()),
        },
    )
}

#[utoipa::path(
    post, path = "/api/v1/auth/pairings", tag = "auth",
    params(("Idempotency-Key" = String, Header, description = "1 to 128 printable ASCII characters")), request_body = dto::PairingBody,
    responses((status = 201, body = dto::PairingCreated), (status = "4XX", body = dto::Problem, content_type = "application/problem+json"))
)]
pub async fn create_pairing(
    State(s): State<AppState>,
    Extension(caller): Extension<Caller>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    raw: Bytes,
) -> Response {
    if s.authenticator().is_none() {
        return disabled();
    }
    let key = tri!(require_key(&headers));
    let req: dto::PairingBody = tri!(body(&raw));
    let purpose = PairingPurpose::from(req.purpose);
    let local_cli = caller
        .0
        .as_ref()
        .is_some_and(|p| p.device_kind == Some(DeviceKind::LocalCli));
    if purpose == PairingPurpose::Setup && !local_cli {
        return ApiError::new(StatusCode::FORBIDDEN, "setup_code_host_only", json!({}))
            .into_response();
    }
    let engine = tri!(s.engine().cloned());
    let respond = |p: Produced<'_>| match p {
        Produced::Pairing(v) => reply(StatusCode::CREATED, &dto::PairingCreated::from_core(v)),
        _ => unexpected(),
    };
    let m = Mutation {
        name: "auth.pairing.create",
        target_type: "pairing",
        target_id: None,
        reason: None,
    };
    let reveal = Reveal::default();
    let slot = &reveal;
    mutate_revealing(
        &s,
        key,
        &method,
        uri.path(),
        &raw,
        m,
        &respond,
        slot,
        |cause, action| async move {
            let issued = engine
                .create_pairing(purpose, &cause, Some(&action))
                .map_err(|e| (e, None))?;
            if let Ok(mut held) = slot.lock() {
                let mut fields = Map::new();
                fields.insert("code".into(), json!(issued.code.expose()));
                *held = Some(fields);
            }
            Ok(None)
        },
    )
    .await
}

#[utoipa::path(
    post, path = "/api/v1/auth/pair", tag = "auth", request_body = dto::PairBody,
    responses((status = 201, body = dto::PairedDevice), (status = "4XX", body = dto::Problem, content_type = "application/problem+json"))
)]
pub async fn pair(State(s): State<AppState>, raw: Bytes) -> Response {
    if s.authenticator().is_none() {
        return disabled();
    }
    let req: dto::PairBody = tri!(body(&raw));
    let engine = tri!(s.engine());
    let correlation_id = correlation().unwrap_or_else(uuid::Uuid::now_v7);
    let issued = tri!(engine.redeem_device_pairing(&req.code, &req.name, correlation_id));
    ok(
        StatusCode::CREATED,
        &dto::PairedDevice {
            device_id: issued.device.id.0,
            token: issued.token.expose().to_string(),
        },
    )
}

#[utoipa::path(
    get, path = "/api/v1/auth/devices", tag = "auth",
    responses((status = 200, body = dto::AuthDevices), (status = 404, body = dto::Problem, content_type = "application/problem+json"))
)]
pub async fn list_devices(State(s): State<AppState>) -> Response {
    if s.authenticator().is_none() {
        return disabled();
    }
    let engine = tri!(s.engine());
    ok(
        StatusCode::OK,
        &dto::AuthDevices {
            devices: tri!(engine.devices())
                .iter()
                .map(dto::Device::from)
                .collect(),
            credentials: tri!(engine.credentials())
                .iter()
                .map(dto::PasskeyCredential::from)
                .collect(),
        },
    )
}

#[utoipa::path(
    post, path = "/api/v1/auth/devices/{device_id}/revoke", tag = "auth",
    params(("device_id" = uuid::Uuid, Path), ("Idempotency-Key" = String, Header, description = "1 to 128 printable ASCII characters")),
    responses((status = 200, body = dto::Device), (status = "4XX", body = dto::Problem, content_type = "application/problem+json"))
)]
pub async fn revoke_device(
    State(s): State<AppState>,
    Path(id): Path<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    raw: Bytes,
) -> Response {
    if s.authenticator().is_none() {
        return disabled();
    }
    let key = tri!(require_key(&headers));
    let id = DeviceId(tri!(path_id(&id)));
    let engine = tri!(s.engine().cloned());
    let respond = |p: Produced<'_>| match p {
        Produced::Device(v) => reply(StatusCode::OK, &dto::Device::from(v)),
        _ => unexpected(),
    };
    let m = Mutation {
        name: "auth.device.revoke",
        target_type: "device",
        target_id: Some(id.0),
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
                .revoke_device(id, &cause, Some(&action))
                .map(|_| None)
                .map_err(|e| (e, None))
        },
    )
    .await
}
