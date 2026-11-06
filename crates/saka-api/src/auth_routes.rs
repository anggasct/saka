use axum::Extension;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use saka_core::ids::{CredentialId, DeviceId};
use saka_core::vocab::{DeviceKind, PairingPurpose};
use saka_engine::{Engine, Produced, Reply, SessionIssued};
use serde_json::{Map, Value, json};
use webauthn_rs::prelude::{Passkey, PublicKeyCredential, RegisterPublicKeyCredential, Uuid};

use crate::auth::{Authenticator, Caller, CeremonyKind, SESSION_COOKIE};
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

const OPERATOR_USER_HANDLE: Uuid = Uuid::from_u128(0x0000_0000_0000_4000_8000_5a4b_0000_0001);
const OPERATOR_USER_NAME: &str = "operator";

fn refusal(status: StatusCode, code: &str) -> Response {
    ApiError::new(status, code, json!({})).into_response()
}

fn authenticator(s: &AppState) -> Result<&std::sync::Arc<Authenticator>, ApiError> {
    s.authenticator()
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "auth_disabled", json!({})))
}

fn session_cookie(session: &SessionIssued, max_age_secs: i64) -> Option<HeaderValue> {
    HeaderValue::from_str(&format!(
        "{SESSION_COOKIE}={}; HttpOnly; Secure; SameSite=Strict; Path=/; Max-Age={max_age_secs}",
        session.token.expose()
    ))
    .ok()
}

fn cleared_cookie() -> HeaderValue {
    HeaderValue::from_static("saka_session=; HttpOnly; Secure; SameSite=Strict; Path=/; Max-Age=0")
}

fn with_session(mut response: Response, session: &SessionIssued, engine: &Engine) -> Response {
    let seconds = (session.expires_at.0 - engine.now().0).max(0) / 1000;
    match session_cookie(session, seconds) {
        Some(value) => {
            response.headers_mut().insert(header::SET_COOKIE, value);
            response
        }
        None => ApiError::internal().into_response(),
    }
}

fn session_ttl_ms(auth: &Authenticator) -> i64 {
    i64::from(auth.settings().session_hours) * 3_600_000
}

fn parse_passkeys(stored: Vec<(CredentialId, Value)>) -> Vec<(CredentialId, Passkey)> {
    stored
        .into_iter()
        .filter_map(|(id, json)| serde_json::from_value(json).ok().map(|p| (id, p)))
        .collect()
}

#[utoipa::path(
    post, path = "/api/v1/auth/passkeys/register/start", tag = "auth", request_body = dto::PasskeyRegisterStartBody,
    responses((status = 200, body = dto::CeremonyStarted), (status = "4XX", body = dto::Problem, content_type = "application/problem+json"))
)]
pub async fn register_start(
    State(s): State<AppState>,
    Extension(caller): Extension<Caller>,
    raw: Bytes,
) -> Response {
    let auth = match authenticator(&s) {
        Ok(a) => a,
        Err(e) => return e.into_response(),
    };
    let req: dto::PasskeyRegisterStartBody = tri!(body(&raw));
    let engine = tri!(s.engine());
    if !saka_core::identity::name_valid(&req.name) {
        return ApiError::validation("name", "invalid").into_response();
    }
    let signed_in = caller.0.as_ref().is_some_and(|p| p.session.is_some());
    let code = match (&req.code, signed_in) {
        (_, true) => None,
        (Some(code), false) => {
            tri!(
                engine.check_pairing_code(code, &[PairingPurpose::Setup, PairingPurpose::Passkey])
            );
            Some(saka_core::secret::SecretText::new(code.clone()))
        }
        (None, false) => {
            return ApiError::from(saka_engine::EngineError::Rejected(
                saka_core::rejection::Rejection::PairingCodeInvalid,
            ))
            .into_response();
        }
    };
    let existing = parse_passkeys(tri!(engine.passkeys()));
    let exclude = existing
        .iter()
        .map(|(_, p)| p.cred_id().clone())
        .collect::<Vec<_>>();
    let (options, state) = match auth.webauthn().start_passkey_registration(
        OPERATOR_USER_HANDLE,
        OPERATOR_USER_NAME,
        &req.name,
        (!exclude.is_empty()).then_some(exclude),
    ) {
        Ok(started) => started,
        Err(e) => {
            tracing::warn!(error = ?e, "passkey registration could not start");
            return ApiError::internal().into_response();
        }
    };
    let id = tri!(engine.ceremony_id());
    auth.begin(
        id.clone(),
        CeremonyKind::Register {
            state: Box::new(state),
            name: req.name,
            code,
        },
        engine.now().0,
    );
    ok(
        StatusCode::OK,
        &dto::CeremonyStarted {
            ceremony_id: id,
            options: serde_json::to_value(options).unwrap_or(Value::Null),
        },
    )
}

#[utoipa::path(
    post, path = "/api/v1/auth/passkeys/register/finish", tag = "auth", request_body = dto::CeremonyFinishBody,
    responses((status = 201, body = dto::PasskeyCredential), (status = "4XX", body = dto::Problem, content_type = "application/problem+json"))
)]
pub async fn register_finish(State(s): State<AppState>, raw: Bytes) -> Response {
    let auth = match authenticator(&s) {
        Ok(a) => a,
        Err(e) => return e.into_response(),
    };
    let req: dto::CeremonyFinishBody = tri!(body(&raw));
    let engine = tri!(s.engine());
    let Some(CeremonyKind::Register { state, name, code }) =
        auth.finish(&req.ceremony_id, engine.now().0)
    else {
        return refusal(StatusCode::BAD_REQUEST, "ceremony_invalid");
    };
    let Ok(credential) = serde_json::from_value::<RegisterPublicKeyCredential>(req.credential)
    else {
        return refusal(StatusCode::BAD_REQUEST, "passkey_rejected");
    };
    let Ok(passkey) = auth
        .webauthn()
        .finish_passkey_registration(&credential, &state)
    else {
        return refusal(StatusCode::BAD_REQUEST, "passkey_rejected");
    };
    let known = parse_passkeys(tri!(engine.passkeys()));
    if known.iter().any(|(_, p)| p.cred_id() == passkey.cred_id()) {
        return refusal(StatusCode::BAD_REQUEST, "passkey_rejected");
    }
    let Ok(stored) = serde_json::to_value(&passkey) else {
        return ApiError::internal().into_response();
    };
    let correlation_id = correlation().unwrap_or_else(uuid::Uuid::now_v7);
    let registered = tri!(engine.register_credential(
        &name,
        &stored,
        code.as_ref().map(saka_core::secret::SecretText::expose),
        session_ttl_ms(auth),
        correlation_id,
    ));
    let response = ok(
        StatusCode::CREATED,
        &dto::PasskeyCredential::from(&registered.credential),
    );
    with_session(response, &registered.session, engine)
}

#[utoipa::path(
    post, path = "/api/v1/auth/login/start", tag = "auth", request_body = dto::LoginStartBody,
    responses((status = 200, body = dto::CeremonyStarted), (status = "4XX", body = dto::Problem, content_type = "application/problem+json"))
)]
pub async fn login_start(State(s): State<AppState>, raw: Bytes) -> Response {
    let auth = match authenticator(&s) {
        Ok(a) => a,
        Err(e) => return e.into_response(),
    };
    let _: dto::LoginStartBody = tri!(body(&raw));
    let engine = tri!(s.engine());
    let passkeys: Vec<Passkey> = parse_passkeys(tri!(engine.passkeys()))
        .into_iter()
        .map(|(_, p)| p)
        .collect();
    if passkeys.is_empty() {
        return refusal(StatusCode::CONFLICT, "setup_required");
    }
    let (options, state) = match auth.webauthn().start_passkey_authentication(&passkeys) {
        Ok(started) => started,
        Err(e) => {
            tracing::warn!(error = ?e, "sign-in could not start");
            return ApiError::internal().into_response();
        }
    };
    let id = tri!(engine.ceremony_id());
    auth.begin(
        id.clone(),
        CeremonyKind::Login {
            state: Box::new(state),
        },
        engine.now().0,
    );
    ok(
        StatusCode::OK,
        &dto::CeremonyStarted {
            ceremony_id: id,
            options: serde_json::to_value(options).unwrap_or(Value::Null),
        },
    )
}

#[utoipa::path(
    post, path = "/api/v1/auth/login/finish", tag = "auth", request_body = dto::CeremonyFinishBody,
    responses((status = 204), (status = "4XX", body = dto::Problem, content_type = "application/problem+json"))
)]
pub async fn login_finish(State(s): State<AppState>, raw: Bytes) -> Response {
    let auth = match authenticator(&s) {
        Ok(a) => a,
        Err(e) => return e.into_response(),
    };
    let req: dto::CeremonyFinishBody = tri!(body(&raw));
    let engine = tri!(s.engine());
    let Some(CeremonyKind::Login { state }) = auth.finish(&req.ceremony_id, engine.now().0) else {
        return refusal(StatusCode::BAD_REQUEST, "ceremony_invalid");
    };
    let invalid = || refusal(StatusCode::UNAUTHORIZED, "auth_invalid");
    let Ok(credential) = serde_json::from_value::<PublicKeyCredential>(req.credential) else {
        return invalid();
    };
    let Ok(result) = auth
        .webauthn()
        .finish_passkey_authentication(&credential, &state)
    else {
        return invalid();
    };
    let mut known = parse_passkeys(tri!(engine.passkeys()));
    let Some((credential_id, passkey)) = known
        .iter_mut()
        .find(|(_, p)| p.cred_id() == result.cred_id())
    else {
        return invalid();
    };
    let update = (passkey.update_credential(&result) == Some(true))
        .then(|| serde_json::to_value(&*passkey).ok())
        .flatten();
    let correlation_id = correlation().unwrap_or_else(uuid::Uuid::now_v7);
    let session = match engine.login(
        *credential_id,
        update.as_ref(),
        session_ttl_ms(auth),
        correlation_id,
    ) {
        Ok(session) => session,
        Err(saka_engine::EngineError::Rejected(_)) => return invalid(),
        Err(e) => return ApiError::from(e).into_response(),
    };
    with_session(StatusCode::NO_CONTENT.into_response(), &session, engine)
}

#[utoipa::path(
    post, path = "/api/v1/auth/logout", tag = "auth",
    responses((status = 204), (status = "4XX", body = dto::Problem, content_type = "application/problem+json"))
)]
pub async fn logout(State(s): State<AppState>, Extension(caller): Extension<Caller>) -> Response {
    if s.authenticator().is_none() {
        return disabled();
    }
    let Some(principal) = caller.0.filter(|p| p.session.is_some()) else {
        return refusal(StatusCode::FORBIDDEN, "session_required");
    };
    let engine = tri!(s.engine());
    let cause = engine.cause(&principal.actor.to_string());
    tri!(engine.close_session(&principal, &cause));
    let mut response = StatusCode::NO_CONTENT.into_response();
    response
        .headers_mut()
        .insert(header::SET_COOKIE, cleared_cookie());
    response
}

#[utoipa::path(
    post, path = "/api/v1/auth/credentials/{credential_id}/revoke", tag = "auth",
    params(("credential_id" = uuid::Uuid, Path), ("Idempotency-Key" = String, Header, description = "1 to 128 printable ASCII characters")),
    responses((status = 200, body = dto::PasskeyCredential), (status = "4XX", body = dto::Problem, content_type = "application/problem+json"))
)]
pub async fn revoke_credential(
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
    let id = CredentialId(tri!(path_id(&id)));
    let engine = tri!(s.engine().cloned());
    let respond = |p: Produced<'_>| match p {
        Produced::Credential(v) => reply(StatusCode::OK, &dto::PasskeyCredential::from(v)),
        _ => unexpected(),
    };
    let m = Mutation {
        name: "auth.credential.revoke",
        target_type: "credential",
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
                .revoke_credential(id, &cause, Some(&action))
                .map(|_| None)
                .map_err(|e| (e, None))
        },
    )
    .await
}
