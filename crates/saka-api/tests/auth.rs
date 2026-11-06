#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests fail loudly on a broken setup"
)]

mod support;

use axum::http::StatusCode;
use serde_json::{Value, json};
use support::{HOST, Logs, ORIGIN, gate, gate_with, get, post, problem};

#[test]
fn without_an_auth_section_the_gate_is_off_and_the_auth_routes_do_not_exist() {
    let g = gate_with(false, |_| {});
    let r = g.send(get("/api/v1/projects").host("127.0.0.1:7420"));
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    for call in [
        get("/api/v1/auth/status"),
        get("/api/v1/auth/devices"),
        post("/api/v1/auth/pair", json!({ "code": "x", "name": "y" })),
    ] {
        let r = g.send(call.host("127.0.0.1:7420"));
        problem(&r, StatusCode::NOT_FOUND, "auth_disabled");
    }
    let r = g.send(get("/api/v1/projects").host(HOST));
    problem(&r, StatusCode::FORBIDDEN, "host_not_allowed");
}

#[test]
fn with_auth_every_request_needs_a_credential_even_from_loopback() {
    let g = gate();
    for host in ["127.0.0.1:7420", "localhost:7420", HOST] {
        for path in [
            "/api/v1/projects",
            "/api/v1/events",
            "/api/v1/diagnostics",
            "/api/v1/auth/devices",
            "/api/v1/nothing-here",
            "/metrics",
        ] {
            let r = g.send(get(path).host(host).peer("127.0.0.1:4000"));
            problem(&r, StatusCode::UNAUTHORIZED, "auth_required");
        }
    }
    let r = g.send(
        post("/api/v1/projects", json!({}))
            .host("127.0.0.1:7420")
            .key("k"),
    );
    problem(&r, StatusCode::UNAUTHORIZED, "auth_required");
}

#[test]
fn the_open_routes_stay_open_with_auth_on() {
    let g = gate();
    for path in ["/healthz", "/api/v1/meta", "/", "/settings/some-project"] {
        let r = g.send(get(path).host("127.0.0.1:7420"));
        assert!(
            r.status != StatusCode::UNAUTHORIZED && r.status != StatusCode::FORBIDDEN,
            "{path}: {}",
            r.status
        );
    }
    let r = g.send(get("/readyz"));
    assert!(r.status == StatusCode::OK || r.status == StatusCode::SERVICE_UNAVAILABLE);
    let r = g.send(get("/api/v1/auth/status"));
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.body["enabled"], true);
    assert_eq!(r.body["authenticated"], false);
    assert_eq!(r.body["actor"], Value::Null);
    assert_eq!(r.body["setup_required"], true);
}

#[test]
fn the_host_must_be_loopback_or_the_origin_host() {
    let g = gate();
    let token = g.cli_token();
    for ok in [HOST, "saka.example.com:443", "127.0.0.1:7420", "localhost"] {
        let r = g.send(get("/api/v1/projects").host(ok).bearer(&token));
        assert_eq!(r.status, StatusCode::OK, "{ok}: {}", r.text);
    }
    for bad in [
        "evil.example.com",
        "saka.example.com:8443",
        "saka.example.com.evil.net",
        "example.com",
        "10.0.0.1:7420",
    ] {
        let r = g.send(get("/api/v1/projects").host(bad).bearer(&token));
        problem(&r, StatusCode::FORBIDDEN, "host_not_allowed");
    }
}

#[test]
fn a_device_token_passes_and_its_actor_is_on_the_events() {
    let g = gate();
    let cli = g.cli_token();
    let (device_id, token) = g.paired_device(&cli, "Pixel");
    let r = g.send(get("/api/v1/auth/status").bearer(&token));
    assert_eq!(r.body["authenticated"], true);
    assert_eq!(r.body["actor"], format!("operator:device:{device_id}"));
    let r = g.send(
        post("/api/v1/auth/pairings", json!({ "purpose": "token" }))
            .bearer(&token)
            .key("by-pixel"),
    );
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let events = g.events();
    let created = events
        .iter()
        .rev()
        .find(|e| e.kind == "identity.pairing.created")
        .unwrap();
    assert_eq!(created.actor, format!("operator:device:{device_id}"));
    let paired = events
        .iter()
        .find(|e| e.kind == "identity.device.paired" && e.payload["name"] == "Pixel")
        .unwrap();
    assert_eq!(paired.actor, format!("operator:device:{device_id}"));
}

#[test]
fn a_wrong_or_revoked_token_is_auth_invalid_and_never_another_identity() {
    let g = gate();
    let cli = g.cli_token();
    let (device_id, token) = g.paired_device(&cli, "Pixel");
    let wrong = format!("sdt_{}", "0".repeat(64));
    for bad in [wrong.as_str(), "sdt_short", "garbage"] {
        let r = g.send(get("/api/v1/projects").bearer(bad));
        problem(&r, StatusCode::UNAUTHORIZED, "auth_invalid");
    }
    let r = g.send(
        get("/api/v1/projects")
            .header("authorization", "Basic abc")
            .header("cookie", "saka_session=x"),
    );
    problem(&r, StatusCode::UNAUTHORIZED, "auth_invalid");
    let r = g.send(get("/api/v1/projects").bearer(&wrong).bearer(&token));
    problem(&r, StatusCode::UNAUTHORIZED, "auth_invalid");

    let r = g.send(get("/api/v1/projects").bearer(&token));
    assert_eq!(r.status, StatusCode::OK);
    let r = g.send(
        post(
            &format!("/api/v1/auth/devices/{device_id}/revoke"),
            json!({}),
        )
        .bearer(&cli)
        .key("revoke-1"),
    );
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let r = g.send(get("/api/v1/projects").bearer(&token));
    problem(&r, StatusCode::UNAUTHORIZED, "auth_invalid");
    let r = g.send(get("/api/v1/auth/status").bearer(&token));
    assert_eq!(r.body["authenticated"], false);
}

#[test]
fn the_local_cli_device_cannot_be_revoked() {
    let g = gate();
    let cli = g.cli_token();
    let r = g.send(get("/api/v1/auth/devices").bearer(&cli));
    let local = r.body["devices"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["kind"] == "local_cli")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let r = g.send(
        post(&format!("/api/v1/auth/devices/{local}/revoke"), json!({}))
            .bearer(&cli)
            .key("k"),
    );
    problem(&r, StatusCode::CONFLICT, "device_not_revocable");
    assert_eq!(
        g.send(get("/api/v1/projects").bearer(&cli)).status,
        StatusCode::OK
    );
}

#[test]
fn the_local_token_is_replaced_every_time_the_server_issues_one() {
    let g = gate();
    let first = g.cli_token();
    let second = g.cli_token();
    assert_ne!(first, second);
    problem(
        &g.send(get("/api/v1/projects").bearer(&first)),
        StatusCode::UNAUTHORIZED,
        "auth_invalid",
    );
    assert_eq!(
        g.send(get("/api/v1/projects").bearer(&second)).status,
        StatusCode::OK
    );
    let r = g.send(get("/api/v1/auth/devices").bearer(&second));
    let locals = r.body["devices"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|d| d["kind"] == "local_cli")
        .count();
    assert_eq!(locals, 1);
}

#[test]
fn only_the_local_cli_can_ask_for_a_setup_code() {
    let g = gate();
    let cli = g.cli_token();
    let (_, client) = g.paired_device(&cli, "Pixel");
    let r = g.send(
        post("/api/v1/auth/pairings", json!({ "purpose": "setup" }))
            .bearer(&client)
            .key("s1"),
    );
    problem(&r, StatusCode::FORBIDDEN, "setup_code_host_only");
    let r = g.send(
        post("/api/v1/auth/pairings", json!({ "purpose": "setup" }))
            .bearer(&cli)
            .key("s2"),
    );
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    assert_eq!(r.body["purpose"], "setup");
    assert_eq!(r.body["code"].as_str().unwrap().len(), 10);
}

#[test]
fn a_pairing_code_shows_once_and_a_replay_of_the_request_does_not_carry_it() {
    let g = gate();
    let cli = g.cli_token();
    let first = g.send(
        post("/api/v1/auth/pairings", json!({ "purpose": "token" }))
            .bearer(&cli)
            .key("same"),
    );
    assert_eq!(first.status, StatusCode::CREATED);
    assert!(first.body["code"].is_string());
    let replay = g.send(
        post("/api/v1/auth/pairings", json!({ "purpose": "token" }))
            .bearer(&cli)
            .key("same"),
    );
    assert_eq!(replay.status, StatusCode::CREATED);
    assert_eq!(replay.headers["idempotent-replayed"], "true");
    assert_eq!(replay.body["id"], first.body["id"]);
    assert!(replay.body.get("code").is_none());
    let reused = g.send(
        post("/api/v1/auth/pairings", json!({ "purpose": "passkey" }))
            .bearer(&cli)
            .key("same"),
    );
    problem(
        &reused,
        StatusCode::UNPROCESSABLE_ENTITY,
        "idempotency_key_reused",
    );
    let missing = g.send(post("/api/v1/auth/pairings", json!({ "purpose": "token" })).bearer(&cli));
    problem(
        &missing,
        StatusCode::BAD_REQUEST,
        "idempotency_key_required",
    );
}

#[test]
fn an_idempotency_key_belongs_to_one_actor() {
    let g = gate();
    let cli = g.cli_token();
    let (_, other) = g.paired_device(&cli, "Pixel");
    let a = g.send(
        post("/api/v1/auth/pairings", json!({ "purpose": "token" }))
            .bearer(&cli)
            .key("shared"),
    );
    let b = g.send(
        post("/api/v1/auth/pairings", json!({ "purpose": "token" }))
            .bearer(&other)
            .key("shared"),
    );
    assert_eq!(a.status, StatusCode::CREATED);
    assert_eq!(b.status, StatusCode::CREATED);
    assert_ne!(a.body["id"], b.body["id"]);
    assert!(b.headers.get("idempotent-replayed").is_none());
}

#[test]
fn a_pairing_code_works_once_and_one_message_covers_every_failure() {
    let g = gate();
    let cli = g.cli_token();
    let code = g.pairing_code(&cli, "token", "p1");
    let typed = format!("{}-{}", code[..5].to_lowercase(), &code[5..]);
    let r = g.send(post(
        "/api/v1/auth/pair",
        json!({ "code": typed, "name": "Laptop" }),
    ));
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    assert!(r.body["token"].as_str().unwrap().starts_with("sdt_"));

    let used = g.send(post(
        "/api/v1/auth/pair",
        json!({ "code": code, "name": "Again" }),
    ));
    problem(&used, StatusCode::BAD_REQUEST, "pairing_code_invalid");
    let unknown = g.send(post(
        "/api/v1/auth/pair",
        json!({ "code": "ZZZZZZZZZZ", "name": "x" }),
    ));
    problem(&unknown, StatusCode::BAD_REQUEST, "pairing_code_invalid");
    let garbage = g.send(post(
        "/api/v1/auth/pair",
        json!({ "code": "!!", "name": "x" }),
    ));
    problem(&garbage, StatusCode::BAD_REQUEST, "pairing_code_invalid");
    assert_eq!(used.body["params"], unknown.body["params"]);
    assert_eq!(used.body["title"], garbage.body["title"]);

    let setup = g.pairing_code(&cli, "setup", "p2");
    let wrong_purpose = g.send(post(
        "/api/v1/auth/pair",
        json!({ "code": setup, "name": "x" }),
    ));
    problem(
        &wrong_purpose,
        StatusCode::BAD_REQUEST,
        "pairing_code_invalid",
    );
}

#[test]
fn a_pairing_code_expires_after_five_minutes_and_a_setup_code_after_fifteen() {
    let g = gate();
    let cli = g.cli_token();
    let short = g.pairing_code(&cli, "token", "p1");
    let setup = g.pairing_code(&cli, "setup", "p2");
    g.sim.clock.advance(5 * 60 * 1000 + 1);
    let r = g.send(post(
        "/api/v1/auth/pair",
        json!({ "code": short, "name": "x" }),
    ));
    problem(&r, StatusCode::BAD_REQUEST, "pairing_code_invalid");
    let still = g
        .sim
        .engine()
        .claim_pairing(&setup, &[saka_core::vocab::PairingPurpose::Setup]);
    assert!(still.is_ok());
    let late = g.pairing_code(&cli, "setup", "p3");
    g.sim.clock.advance(15 * 60 * 1000 + 1);
    let r = g
        .sim
        .engine()
        .claim_pairing(&late, &[saka_core::vocab::PairingPurpose::Setup]);
    assert!(r.is_err());
}

#[test]
fn five_wrong_codes_burn_every_live_code() {
    let g = gate();
    let cli = g.cli_token();
    let code = g.pairing_code(&cli, "token", "p1");
    for i in 0..5 {
        let r = g.send(
            post(
                "/api/v1/auth/pair",
                json!({ "code": "ZZZZZZZZZZ", "name": "x" }),
            )
            .peer(&format!("10.9.0.{i}:1")),
        );
        problem(&r, StatusCode::BAD_REQUEST, "pairing_code_invalid");
    }
    let r = g.send(post(
        "/api/v1/auth/pair",
        json!({ "code": code, "name": "Laptop" }),
    ));
    problem(&r, StatusCode::BAD_REQUEST, "pairing_code_invalid");
    let fresh = g.pairing_code(&cli, "token", "p2");
    let r = g.send(post(
        "/api/v1/auth/pair",
        json!({ "code": fresh, "name": "Laptop" }),
    ));
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
}

#[test]
fn a_bad_device_name_does_not_burn_the_code() {
    let g = gate();
    let cli = g.cli_token();
    let code = g.pairing_code(&cli, "token", "p1");
    for name in ["", &"a".repeat(101)] {
        let r = g.send(post(
            "/api/v1/auth/pair",
            json!({ "code": code, "name": name }),
        ));
        problem(&r, StatusCode::UNPROCESSABLE_ENTITY, "validation_failed");
    }
    let r = g.send(post(
        "/api/v1/auth/pair",
        json!({ "code": code, "name": "Fine" }),
    ));
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
}

#[test]
fn the_eleventh_failure_in_a_minute_is_429_with_retry_after_and_forwarded_for_is_ignored() {
    let g = gate();
    let attempt = |peer: &str, forwarded: &str| {
        g.send(
            post(
                "/api/v1/auth/pair",
                json!({ "code": "ZZZZZZZZZZ", "name": "x" }),
            )
            .peer(peer)
            .header("x-forwarded-for", forwarded),
        )
    };
    for i in 0..10 {
        let r = attempt("10.7.7.7:1", &format!("203.0.113.{i}"));
        problem(&r, StatusCode::BAD_REQUEST, "pairing_code_invalid");
    }
    let r = attempt("10.7.7.7:2", "198.51.100.1");
    problem(&r, StatusCode::TOO_MANY_REQUESTS, "auth_rate_limited");
    let retry: u64 = r.headers["retry-after"].to_str().unwrap().parse().unwrap();
    assert!((1..=60).contains(&retry));

    let other = attempt("10.8.8.8:1", "10.7.7.7");
    problem(&other, StatusCode::BAD_REQUEST, "pairing_code_invalid");

    g.sim.clock.advance(61_000);
    let r = attempt("10.7.7.7:3", "203.0.113.1");
    problem(&r, StatusCode::BAD_REQUEST, "pairing_code_invalid");
}

#[test]
fn a_missing_peer_address_shares_one_bucket() {
    let g = gate();
    for _ in 0..10 {
        let mut call = post(
            "/api/v1/auth/pair",
            json!({ "code": "ZZZZZZZZZZ", "name": "x" }),
        );
        call.peer = None;
        problem(
            &g.send(call),
            StatusCode::BAD_REQUEST,
            "pairing_code_invalid",
        );
    }
    let mut call = post(
        "/api/v1/auth/pair",
        json!({ "code": "ZZZZZZZZZZ", "name": "x" }),
    );
    call.peer = None;
    problem(
        &g.send(call),
        StatusCode::TOO_MANY_REQUESTS,
        "auth_rate_limited",
    );
}

#[test]
fn a_post_with_a_cookie_needs_a_listed_origin_and_a_bearer_post_does_not() {
    let g = gate();
    let token = g.cli_token();
    let cookie = "saka_session=sss_abc";
    let r = g.send(
        post("/api/v1/auth/pairings", json!({ "purpose": "token" }))
            .header("cookie", cookie)
            .key("a"),
    );
    problem(&r, StatusCode::FORBIDDEN, "origin_not_allowed");
    let r = g.send(
        post("/api/v1/auth/pairings", json!({ "purpose": "token" }))
            .header("cookie", cookie)
            .header("origin", "https://evil.example.net")
            .key("b"),
    );
    problem(&r, StatusCode::FORBIDDEN, "origin_not_allowed");
    for origin in [ORIGIN, "https://app.example.com"] {
        let r = g.send(
            post("/api/v1/auth/pairings", json!({ "purpose": "token" }))
                .header("cookie", cookie)
                .header("origin", origin)
                .key("c"),
        );
        problem(&r, StatusCode::UNAUTHORIZED, "auth_invalid");
    }
    let r = g.send(
        post("/api/v1/auth/pairings", json!({ "purpose": "token" }))
            .bearer(&token)
            .key("d"),
    );
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let r = g.send(
        post("/api/v1/auth/pairings", json!({ "purpose": "token" }))
            .bearer(&token)
            .header("origin", "https://evil.example.net")
            .key("e"),
    );
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let r = g.send(
        post(
            "/api/v1/auth/pair",
            json!({ "code": "ZZZZZZZZZZ", "name": "x" }),
        )
        .header("origin", "https://evil.example.net"),
    );
    problem(&r, StatusCode::FORBIDDEN, "origin_not_allowed");
}

#[test]
fn metrics_need_a_credential_with_auth_on() {
    let g = gate();
    problem(
        &g.send(get("/metrics").host("127.0.0.1:7420")),
        StatusCode::UNAUTHORIZED,
        "auth_required",
    );
    let token = g.cli_token();
    let r = g.send(get("/metrics").host("127.0.0.1:7420").bearer(&token));
    assert_eq!(r.status, StatusCode::OK);
}

#[test]
fn auth_responses_are_never_cached() {
    let g = gate();
    let r = g.send(get("/api/v1/auth/status"));
    assert_eq!(r.headers["cache-control"], "no-store");
    let token = g.cli_token();
    let r = g.send(
        post("/api/v1/auth/pairings", json!({ "purpose": "token" }))
            .bearer(&token)
            .key("k"),
    );
    assert_eq!(r.headers["cache-control"], "no-store");
}

#[test]
fn no_token_code_or_hash_reaches_the_logs_events_actions_or_error_bodies() {
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
        let cli = g.cli_token();
        let mut secrets = vec![cli.clone()];
        let mut bodies = Vec::new();
        let code = g.pairing_code(&cli, "token", "p1");
        secrets.push(code.clone());
        let setup = g.pairing_code(&cli, "setup", "p2");
        secrets.push(setup);
        let paired = g.send(post(
            "/api/v1/auth/pair",
            json!({ "code": code, "name": "Pixel" }),
        ));
        let device_token = paired.body["token"].as_str().unwrap().to_string();
        secrets.push(device_token.clone());
        let wrong = format!("sdt_{}", "1".repeat(64));
        for call in [
            get("/api/v1/projects").bearer(&wrong),
            get("/api/v1/projects").bearer("bearer-garbage"),
            get("/api/v1/projects").header("cookie", "saka_session=sss_leakme"),
            post(
                "/api/v1/auth/pair",
                json!({ "code": code, "name": "Again" }),
            ),
            post(
                "/api/v1/auth/pair",
                json!({ "code": "QQQQQQQQQQ", "name": "x" }),
            ),
            get("/api/v1/projects").bearer(&device_token),
            get("/api/v1/auth/devices").bearer(&device_token),
            post("/api/v1/projects", json!({ "nope": "sdt_embedded" }))
                .bearer(&cli)
                .key("q"),
        ] {
            bodies.push(g.send(call).text);
        }
        secrets.push(wrong);
        secrets.push("sss_leakme".into());
        secrets.push("QQQQQQQQQQ".into());
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
                "SELECT response_body || actor || coalesce(reason, '') FROM action_request",
            )?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })
        .unwrap();
    haystack.extend(stored);
    assert!(logs.text().contains("\"message\":\"request\""));
    assert!(haystack.contains("auth_invalid"));
    assert!(
        g.events()
            .iter()
            .any(|e| e.kind == "identity.device.paired")
    );
    for secret in secrets {
        let digest = saka_engine::digest(&secret);
        assert!(!haystack.contains(&secret), "{secret} leaked");
        assert!(!haystack.contains(&digest), "the hash of {secret} leaked");
    }
}
