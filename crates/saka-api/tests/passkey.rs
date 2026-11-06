#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests fail loudly on a broken setup"
)]

mod support;

use axum::http::StatusCode;
use serde_json::{Value, json};
use support::softtoken::SoftToken;
use support::{Gate, Logs, ORIGIN, Reply, gate, get, post, problem};

const RP_ID: &str = "example.com";

fn token(seed: u8) -> SoftToken {
    SoftToken::new(seed, RP_ID, ORIGIN)
}

fn cookie_of(r: &Reply) -> String {
    let header = r.headers["set-cookie"].to_str().unwrap();
    header.split(';').next().unwrap().to_string()
}

impl Gate {
    fn setup_code(&self, cli: &str, key: &str) -> String {
        self.pairing_code(cli, "setup", key)
    }

    fn register(
        &self,
        token: &mut SoftToken,
        code: Option<&str>,
        cookie: Option<&str>,
        name: &str,
    ) -> Reply {
        let mut body = json!({ "name": name });
        if let Some(code) = code {
            body["code"] = json!(code);
        }
        let mut start = post("/api/v1/auth/passkeys/register/start", body).header("origin", ORIGIN);
        if let Some(cookie) = cookie {
            start = start.header("cookie", cookie);
        }
        let started = self.send(start);
        if started.status != StatusCode::OK {
            return started;
        }
        let credential = token.create(&started.body["options"]);
        let mut finish = post(
            "/api/v1/auth/passkeys/register/finish",
            json!({ "ceremony_id": started.body["ceremony_id"], "credential": credential }),
        )
        .header("origin", ORIGIN);
        if let Some(cookie) = cookie {
            finish = finish.header("cookie", cookie);
        }
        self.send(finish)
    }

    fn login_ceremony(&self) -> Value {
        let started =
            self.send(post("/api/v1/auth/login/start", json!({})).header("origin", ORIGIN));
        assert_eq!(started.status, StatusCode::OK, "{}", started.text);
        started.body
    }

    fn login_finish(&self, ceremony_id: &Value, credential: Value) -> Reply {
        self.send(
            post(
                "/api/v1/auth/login/finish",
                json!({ "ceremony_id": ceremony_id, "credential": credential }),
            )
            .header("origin", ORIGIN),
        )
    }

    fn login(&self, token: &mut SoftToken) -> Reply {
        let ceremony = self.login_ceremony();
        let credential = token.get(&ceremony["options"]);
        self.login_finish(&ceremony["ceremony_id"], credential)
    }

    fn first_passkey(&self, token: &mut SoftToken) -> (String, String) {
        let cli = self.cli_token();
        let code = self.setup_code(&cli, "setup-1");
        let r = self.register(token, Some(&code), None, "Laptop");
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
        (r.body["id"].as_str().unwrap().to_string(), cookie_of(&r))
    }
}

#[test]
fn a_setup_code_registers_the_first_passkey_and_opens_a_hardened_session() {
    let g = gate();
    assert_eq!(
        g.send(get("/api/v1/auth/status")).body["setup_required"],
        true
    );
    let cli = g.cli_token();
    let code = g.setup_code(&cli, "s1");
    let mut laptop = token(1);
    let r = g.register(&mut laptop, Some(&code), None, "Laptop");
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    assert_eq!(r.body["name"], "Laptop");
    let set_cookie = r.headers["set-cookie"].to_str().unwrap().to_string();
    for part in [
        "saka_session=sss_",
        "HttpOnly",
        "Secure",
        "SameSite=Strict",
        "Path=/",
        "Max-Age=604800",
    ] {
        assert!(set_cookie.contains(part), "{part} missing in {set_cookie}");
    }
    let id = r.body["id"].as_str().unwrap();
    let status = g.send(get("/api/v1/auth/status").header("cookie", cookie_of(&r)));
    assert_eq!(status.body["authenticated"], true);
    assert_eq!(status.body["actor"], format!("operator:passkey:{id}"));
    assert_eq!(status.body["setup_required"], false);

    let again = g.register(&mut token(2), Some(&code), None, "Phone");
    problem(&again, StatusCode::BAD_REQUEST, "pairing_code_invalid");
    let kinds: Vec<String> = g.events().iter().map(|e| e.kind.clone()).collect();
    assert!(kinds.contains(&"identity.credential.registered".to_string()));
    assert!(kinds.contains(&"identity.session.opened".to_string()));
}

#[test]
fn a_registered_passkey_signs_in_and_the_cookie_opens_the_api() {
    let g = gate();
    let mut laptop = token(1);
    let (id, _) = g.first_passkey(&mut laptop);
    let r = g.login(&mut laptop);
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.text);
    let cookie = cookie_of(&r);
    assert!(cookie.starts_with("saka_session=sss_"));
    let projects = g.send(get("/api/v1/projects").header("cookie", &cookie));
    assert_eq!(projects.status, StatusCode::OK, "{}", projects.text);
    let opened = g
        .events()
        .into_iter()
        .rfind(|e| e.kind == "identity.session.opened")
        .unwrap();
    assert_eq!(opened.actor, format!("operator:passkey:{id}"));
    let devices = g.send(get("/api/v1/auth/devices").header("cookie", &cookie));
    assert_eq!(devices.body["credentials"][0]["name"], "Laptop");
    assert!(devices.body["credentials"][0]["last_used_at"].is_string());
}

#[test]
fn registering_needs_a_code_or_a_session() {
    let g = gate();
    let cli = g.cli_token();
    let mut laptop = token(1);
    for body in [
        json!({ "name": "x" }),
        json!({ "name": "x", "code": "ZZZZZZZZZZ" }),
    ] {
        let r = g.send(post("/api/v1/auth/passkeys/register/start", body).header("origin", ORIGIN));
        problem(&r, StatusCode::BAD_REQUEST, "pairing_code_invalid");
    }
    let by_token = g.send(
        post(
            "/api/v1/auth/passkeys/register/start",
            json!({ "name": "x" }),
        )
        .bearer(&cli),
    );
    problem(&by_token, StatusCode::BAD_REQUEST, "pairing_code_invalid");
    let bad_name = g.send(
        post(
            "/api/v1/auth/passkeys/register/start",
            json!({ "name": "", "code": g.setup_code(&cli, "s1") }),
        )
        .header("origin", ORIGIN),
    );
    problem(
        &bad_name,
        StatusCode::UNPROCESSABLE_ENTITY,
        "validation_failed",
    );

    let (_, cookie) = g.first_passkey(&mut laptop);
    let mut phone = token(2);
    let r = g.register(&mut phone, None, Some(&cookie), "Phone");
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let cli = g.cli_token();
    let passkey_code = g.pairing_code(&cli, "passkey", "p1");
    let mut tablet = token(3);
    let r = g.register(&mut tablet, Some(&passkey_code), None, "Tablet");
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let listed = g.send(get("/api/v1/auth/devices").bearer(&cli));
    assert_eq!(listed.body["credentials"].as_array().unwrap().len(), 3);
}

#[test]
fn the_same_authenticator_cannot_be_registered_twice() {
    let g = gate();
    let mut laptop = token(1);
    let (_, cookie) = g.first_passkey(&mut laptop);
    let again = g.register(&mut laptop, None, Some(&cookie), "Laptop again");
    assert_eq!(again.status, StatusCode::BAD_REQUEST, "{}", again.text);
}

#[test]
fn a_ceremony_finishes_once_and_only_within_five_minutes() {
    let g = gate();
    let cli = g.cli_token();
    let code = g.setup_code(&cli, "s1");
    let mut laptop = token(1);
    let started = g.send(
        post(
            "/api/v1/auth/passkeys/register/start",
            json!({ "name": "Laptop", "code": code }),
        )
        .header("origin", ORIGIN),
    );
    let credential = laptop.create(&started.body["options"]);
    let finish = |credential: &Value| {
        g.send(
            post(
                "/api/v1/auth/passkeys/register/finish",
                json!({ "ceremony_id": started.body["ceremony_id"], "credential": credential }),
            )
            .header("origin", ORIGIN),
        )
    };
    assert_eq!(finish(&credential).status, StatusCode::CREATED);
    problem(
        &finish(&credential),
        StatusCode::BAD_REQUEST,
        "ceremony_invalid",
    );
    let unknown = g.send(
        post(
            "/api/v1/auth/passkeys/register/finish",
            json!({ "ceremony_id": "00000000000000000000000000000000", "credential": credential }),
        )
        .header("origin", ORIGIN),
    );
    problem(&unknown, StatusCode::BAD_REQUEST, "ceremony_invalid");

    let ceremony = g.login_ceremony();
    let signed = laptop.get(&ceremony["options"]);
    g.sim.clock.advance(5 * 60 * 1000 + 1);
    problem(
        &g.login_finish(&ceremony["ceremony_id"], signed),
        StatusCode::BAD_REQUEST,
        "ceremony_invalid",
    );

    let ceremony = g.login_ceremony();
    let signed = laptop.get(&ceremony["options"]);
    assert_eq!(
        g.login_finish(&ceremony["ceremony_id"], signed.clone())
            .status,
        StatusCode::NO_CONTENT
    );
    problem(
        &g.login_finish(&ceremony["ceremony_id"], signed),
        StatusCode::BAD_REQUEST,
        "ceremony_invalid",
    );
}

#[test]
fn a_ceremony_is_not_a_login_and_a_login_is_not_a_registration() {
    let g = gate();
    let mut laptop = token(1);
    g.first_passkey(&mut laptop);
    let cli = g.cli_token();
    let code = g.setup_code(&cli, "s2");
    let started = g.send(
        post(
            "/api/v1/auth/passkeys/register/start",
            json!({ "name": "Phone", "code": code }),
        )
        .header("origin", ORIGIN),
    );
    let signed = laptop.get(&g.login_ceremony()["options"]);
    let wrong_kind = g.login_finish(&started.body["ceremony_id"], signed);
    problem(&wrong_kind, StatusCode::BAD_REQUEST, "ceremony_invalid");
}

#[test]
fn another_origin_or_relying_party_is_refused() {
    let g = gate();
    let cli = g.cli_token();
    let evil = |seed| SoftToken::new(seed, RP_ID, "https://evil.example.net");
    let wrong_rp = |seed| SoftToken::new(seed, "other.org", ORIGIN);
    for (n, mut forged) in [evil(5), wrong_rp(6)].into_iter().enumerate() {
        let code = g.setup_code(&cli, &format!("s{n}"));
        let r = g.register(&mut forged, Some(&code), None, "Forged");
        problem(&r, StatusCode::BAD_REQUEST, "passkey_rejected");
    }
    assert_eq!(
        g.send(get("/api/v1/auth/status")).body["setup_required"],
        true
    );

    let mut laptop = token(1);
    g.first_passkey(&mut laptop);
    for mut forged in [evil(1), wrong_rp(1)] {
        let ceremony = g.login_ceremony();
        let signed = forged.get(&ceremony["options"]);
        problem(
            &g.login_finish(&ceremony["ceremony_id"], signed),
            StatusCode::UNAUTHORIZED,
            "auth_invalid",
        );
    }
}

#[test]
fn a_signature_from_another_key_or_a_replayed_counter_is_refused() {
    let g = gate();
    let mut laptop = token(1);
    g.first_passkey(&mut laptop);
    let mut stranger = SoftToken::new(9, RP_ID, ORIGIN);
    let ceremony = g.login_ceremony();
    let mut credential = stranger.get(&ceremony["options"]);
    credential["id"] = json!(laptop_id(&mut laptop));
    credential["rawId"] = credential["id"].clone();
    problem(
        &g.login_finish(&ceremony["ceremony_id"], credential),
        StatusCode::UNAUTHORIZED,
        "auth_invalid",
    );

    laptop.counter = 5;
    assert_eq!(g.login(&mut laptop).status, StatusCode::NO_CONTENT);
    problem(
        &g.login(&mut laptop),
        StatusCode::UNAUTHORIZED,
        "auth_invalid",
    );
    laptop.counter = 6;
    assert_eq!(g.login(&mut laptop).status, StatusCode::NO_CONTENT);
}

fn laptop_id(laptop: &mut SoftToken) -> String {
    let probe = laptop.get(&json!({ "publicKey": { "challenge": "AAAA" } }));
    probe["id"].as_str().unwrap().to_string()
}

#[test]
fn a_session_ends_when_it_expires_or_on_logout_and_logout_needs_a_session() {
    let g = gate();
    let mut laptop = token(1);
    let (_, cookie) = g.first_passkey(&mut laptop);
    let ok = |cookie: &str| {
        g.send(get("/api/v1/projects").header("cookie", cookie))
            .status
    };
    assert_eq!(ok(&cookie), StatusCode::OK);
    g.sim.clock.advance(168 * 3_600_000 - 1);
    assert_eq!(ok(&cookie), StatusCode::OK);
    g.sim.clock.advance(1);
    problem(
        &g.send(get("/api/v1/projects").header("cookie", &cookie)),
        StatusCode::UNAUTHORIZED,
        "auth_invalid",
    );

    let fresh = cookie_of(&g.login(&mut laptop));
    assert_eq!(ok(&fresh), StatusCode::OK);
    let out = g.send(
        post("/api/v1/auth/logout", json!({}))
            .header("cookie", &fresh)
            .header("origin", ORIGIN),
    );
    assert_eq!(out.status, StatusCode::NO_CONTENT, "{}", out.text);
    assert!(
        out.headers["set-cookie"]
            .to_str()
            .unwrap()
            .contains("Max-Age=0")
    );
    problem(
        &g.send(get("/api/v1/projects").header("cookie", &fresh)),
        StatusCode::UNAUTHORIZED,
        "auth_invalid",
    );
    assert!(
        g.events()
            .iter()
            .any(|e| e.kind == "identity.session.closed")
    );

    let cli = g.cli_token();
    let by_token = g.send(post("/api/v1/auth/logout", json!({})).bearer(&cli));
    problem(&by_token, StatusCode::FORBIDDEN, "session_required");
}

#[test]
fn revoking_a_passkey_ends_its_sessions_and_the_last_one_stays() {
    let g = gate();
    let mut laptop = token(1);
    let (laptop_id, laptop_cookie) = g.first_passkey(&mut laptop);
    let mut phone = token(2);
    let r = g.register(&mut phone, None, Some(&laptop_cookie), "Phone");
    let (phone_id, phone_cookie) = (r.body["id"].as_str().unwrap().to_string(), cookie_of(&r));
    let status = |cookie: &str| {
        g.send(get("/api/v1/projects").header("cookie", cookie))
            .status
    };
    assert_eq!(status(&phone_cookie), StatusCode::OK);

    let revoke = |id: &str, cookie: &str, key: &str| {
        g.send(
            post(&format!("/api/v1/auth/credentials/{id}/revoke"), json!({}))
                .header("cookie", cookie)
                .header("origin", ORIGIN)
                .key(key),
        )
    };
    assert_eq!(
        revoke(&phone_id, &laptop_cookie, "r1").status,
        StatusCode::OK
    );
    assert_eq!(status(&phone_cookie), StatusCode::UNAUTHORIZED);
    assert_eq!(status(&laptop_cookie), StatusCode::OK);
    let ceremony = g.login_ceremony();
    let signed = phone.get(&ceremony["options"]);
    problem(
        &g.login_finish(&ceremony["ceremony_id"], signed),
        StatusCode::UNAUTHORIZED,
        "auth_invalid",
    );
    let again = revoke(&phone_id, &laptop_cookie, "r1");
    assert_eq!(again.headers["idempotent-replayed"], "true");

    problem(
        &revoke(&laptop_id, &laptop_cookie, "r2"),
        StatusCode::CONFLICT,
        "last_credential",
    );
    assert_eq!(status(&laptop_cookie), StatusCode::OK);
    problem(
        &revoke("00000000-0000-0000-0000-000000000099", &laptop_cookie, "r3"),
        StatusCode::NOT_FOUND,
        "not_found",
    );
}

#[test]
fn a_wrong_bearer_never_falls_back_to_a_valid_cookie() {
    let g = gate();
    let mut laptop = token(1);
    let (_, cookie) = g.first_passkey(&mut laptop);
    let wrong = format!("sdt_{}", "0".repeat(64));
    let r = g.send(
        get("/api/v1/projects")
            .bearer(&wrong)
            .header("cookie", &cookie),
    );
    problem(&r, StatusCode::UNAUTHORIZED, "auth_invalid");
    let cli = g.cli_token();
    let r = g.send(
        get("/api/v1/auth/status")
            .bearer(&cli)
            .header("cookie", "saka_session=sss_x"),
    );
    assert!(
        r.body["actor"]
            .as_str()
            .unwrap()
            .starts_with("operator:device:")
    );
}

#[test]
fn a_cookie_post_needs_an_allowed_origin_even_with_a_valid_session() {
    let g = gate();
    let mut laptop = token(1);
    let (_, cookie) = g.first_passkey(&mut laptop);
    let attempt = |origin: Option<&str>, key: &str| {
        let mut call = post("/api/v1/auth/pairings", json!({ "purpose": "token" }))
            .header("cookie", &cookie)
            .key(key);
        if let Some(origin) = origin {
            call = call.header("origin", origin);
        }
        g.send(call)
    };
    problem(
        &attempt(None, "a"),
        StatusCode::FORBIDDEN,
        "origin_not_allowed",
    );
    problem(
        &attempt(Some("https://evil.example.net"), "b"),
        StatusCode::FORBIDDEN,
        "origin_not_allowed",
    );
    problem(
        &attempt(Some("http://saka.example.com"), "c"),
        StatusCode::FORBIDDEN,
        "origin_not_allowed",
    );
    assert_eq!(attempt(Some(ORIGIN), "d").status, StatusCode::CREATED);
}

#[test]
fn signing_in_before_any_passkey_exists_asks_for_setup() {
    let g = gate();
    let r = g.send(post("/api/v1/auth/login/start", json!({})).header("origin", ORIGIN));
    problem(&r, StatusCode::CONFLICT, "setup_required");
}

#[test]
fn the_passkey_endpoints_share_the_failure_limit() {
    let g = gate();
    for _ in 0..10 {
        let r = g.send(
            post(
                "/api/v1/auth/passkeys/register/finish",
                json!({ "ceremony_id": "nope", "credential": {} }),
            )
            .header("origin", ORIGIN),
        );
        problem(&r, StatusCode::BAD_REQUEST, "ceremony_invalid");
    }
    let r = g.send(post("/api/v1/auth/login/start", json!({})).header("origin", ORIGIN));
    problem(&r, StatusCode::TOO_MANY_REQUESTS, "auth_rate_limited");
    assert!(r.headers.contains_key("retry-after"));
}

#[test]
fn no_session_cookie_or_ceremony_secret_reaches_the_logs_events_actions_or_error_bodies() {
    let logs = Logs::default();
    let sink = logs.clone();
    let subscriber = tracing_subscriber::fmt()
        .json()
        .with_max_level(tracing::Level::TRACE)
        .with_span_list(true)
        .with_writer(move || sink.clone())
        .finish();
    let (g, secrets, bodies) = tracing::subscriber::with_default(subscriber, || {
        let g = gate();
        let mut laptop = token(1);
        let cli = g.cli_token();
        let code = g.setup_code(&cli, "s1");
        let registered = g.register(&mut laptop, Some(&code), None, "Laptop");
        let first = cookie_of(&registered);
        let signed_in = g.login(&mut laptop);
        let second = cookie_of(&signed_in);
        let mut bodies = vec![registered.text.clone(), signed_in.text.clone()];
        let wrong = "sss_".to_string() + &"7".repeat(64);
        for call in [
            get("/api/v1/projects").header("cookie", format!("saka_session={wrong}")),
            get("/api/v1/projects").header("cookie", &first),
            post("/api/v1/auth/logout", json!({}))
                .header("cookie", &second)
                .header("origin", ORIGIN),
            get("/api/v1/projects").header("cookie", &second),
        ] {
            bodies.push(g.send(call).text);
        }
        let secrets = vec![
            first.trim_start_matches("saka_session=").to_string(),
            second.trim_start_matches("saka_session=").to_string(),
            wrong,
            code,
            cli,
        ];
        (g, secrets, bodies)
    });
    let mut haystack = logs.text();
    for body in bodies {
        haystack.push_str(&body);
    }
    for e in g.events() {
        haystack.push_str(&e.payload.to_string());
        haystack.push_str(&e.actor);
    }
    let stored: Vec<String> = g
        .sim
        .engine()
        .store()
        .read(|c| {
            let mut stmt = c.prepare(
                "SELECT response_body || actor FROM action_request
                 UNION ALL SELECT payload FROM event",
            )?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })
        .unwrap();
    haystack.extend(stored);
    assert!(logs.text().contains("\"message\":\"request\""));
    assert!(haystack.contains("auth_invalid"));
    for secret in secrets {
        assert!(secret.len() >= 10);
        assert!(!haystack.contains(&secret), "{secret} leaked");
        assert!(
            !haystack.contains(&saka_engine::digest(&secret)),
            "the hash of {secret} leaked"
        );
    }
}
