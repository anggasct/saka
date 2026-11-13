use std::collections::BTreeSet;

use hmac::{Hmac, KeyInit, Mac};
use saka_ports::{MAX_WEBHOOK_TARGETS, PortError, PortResult, WebhookDelivery, WebhookTarget};
use serde_json::{Value, json};
use sha2::Sha256;

const SIGNATURE_PREFIX: &str = "sha256=";
const MAX_DELIVERY_ID: usize = 64;
const MAX_EVENT: usize = 64;
const ISSUE_ACTIONS: [&str; 5] = ["opened", "labeled", "closed", "reopened", "edited"];

fn header<'h>(headers: &'h [(String, String)], name: &str) -> Option<&'h str> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

fn refused() -> PortError {
    PortError::permanent("webhook_signature_invalid", json!({}))
}

fn malformed(field: &str) -> PortError {
    PortError::permanent("webhook_malformed", json!({ "field": field }))
}

fn decode_hex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| {
            text.get(i..i + 2)
                .and_then(|b| u8::from_str_radix(b, 16).ok())
        })
        .collect()
}

pub(crate) fn verify(secret: &[u8], headers: &[(String, String)], body: &[u8]) -> PortResult<()> {
    let signature = header(headers, "x-hub-signature-256")
        .and_then(|v| v.strip_prefix(SIGNATURE_PREFIX))
        .and_then(decode_hex)
        .ok_or_else(refused)?;
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).map_err(|_| refused())?;
    mac.update(body);
    mac.verify_slice(&signature).map_err(|_| refused())
}

fn delivery_id(headers: &[(String, String)]) -> PortResult<String> {
    let id = header(headers, "x-github-delivery").ok_or_else(refused)?;
    let valid = !id.is_empty()
        && id.len() <= MAX_DELIVERY_ID
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
    if valid {
        Ok(id.to_string())
    } else {
        Err(malformed("delivery_id"))
    }
}

fn is_sha(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

fn numbers(list: &Value) -> impl Iterator<Item = WebhookTarget> + '_ {
    list.as_array()
        .into_iter()
        .flatten()
        .filter_map(|pr| pr["number"].as_u64())
        .filter(|n| *n >= 1)
        .map(|number| WebhookTarget::Change { number })
}

fn head(sha: &Value) -> Option<WebhookTarget> {
    sha.as_str()
        .filter(|s| is_sha(s))
        .map(|s| WebhookTarget::Head {
            sha: s.to_ascii_lowercase(),
        })
}

fn targets(event: &str, payload: &Value) -> Vec<WebhookTarget> {
    let mut found: BTreeSet<WebhookTarget> = BTreeSet::new();
    match event {
        "pull_request" => {
            if let Some(number) = payload["pull_request"]["number"]
                .as_u64()
                .filter(|n| *n >= 1)
            {
                found.insert(WebhookTarget::Change { number });
            }
            found.insert(WebhookTarget::Intake);
        }
        "issue_comment" => {
            found.insert(if payload["issue"]["pull_request"].is_null() {
                WebhookTarget::IssueIntake
            } else {
                WebhookTarget::Intake
            });
        }
        "issues" if ISSUE_ACTIONS.contains(&payload["action"].as_str().unwrap_or_default()) => {
            found.insert(WebhookTarget::IssueIntake);
        }
        "check_run" | "check_suite" => {
            let object = &payload[event];
            found.extend(head(&object["head_sha"]));
            found.extend(numbers(&object["pull_requests"]));
        }
        "status" => {
            found.extend(head(&payload["sha"]));
        }
        _ => {}
    }
    found.into_iter().take(MAX_WEBHOOK_TARGETS).collect()
}

pub(crate) fn parse(
    secret: &[u8],
    headers: &[(String, String)],
    body: &[u8],
) -> PortResult<WebhookDelivery> {
    let event = header(headers, "x-github-event").ok_or_else(refused)?;
    verify(secret, headers, body)?;
    let delivery_id = delivery_id(headers)?;
    if event.is_empty() || event.len() > MAX_EVENT {
        return Err(malformed("event"));
    }
    let payload: Value = serde_json::from_slice(body).map_err(|_| malformed("body"))?;
    Ok(WebhookDelivery {
        delivery_id,
        event: event.to_string(),
        action: payload["action"].as_str().map(String::from),
        repo_full_name: payload["repository"]["full_name"]
            .as_str()
            .map(String::from),
        targets: targets(event, &payload),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &[u8] = b"0123456789abcdef0123456789abcdef";
    const HEAD: &str = "abcdef0123456789abcdef0123456789abcdef01";

    fn sign(body: &[u8]) -> String {
        let mut mac = Hmac::<Sha256>::new_from_slice(SECRET).unwrap();
        mac.update(body);
        let hex: String = mac
            .finalize()
            .into_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        format!("sha256={hex}")
    }

    fn headers(event: &str, signature: &str) -> Vec<(String, String)> {
        vec![
            ("X-GitHub-Event".into(), event.into()),
            (
                "X-GitHub-Delivery".into(),
                "72d3162e-cc78-11e3-81ab-4c9367dc0958".into(),
            ),
            ("X-Hub-Signature-256".into(), signature.into()),
        ]
    }

    fn parsed(event: &str, payload: &Value) -> WebhookDelivery {
        let body = serde_json::to_vec(payload).unwrap();
        parse(SECRET, &headers(event, &sign(&body)), &body).unwrap()
    }

    #[test]
    fn each_event_names_its_targets() {
        let repo = json!({ "full_name": "acme/app" });
        let pr = parsed(
            "pull_request",
            &json!({ "action": "synchronize", "pull_request": { "number": 12 }, "repository": repo }),
        );
        assert_eq!(
            pr.targets,
            vec![WebhookTarget::Change { number: 12 }, WebhookTarget::Intake]
        );
        assert_eq!(pr.action.as_deref(), Some("synchronize"));
        assert_eq!(pr.repo_full_name.as_deref(), Some("acme/app"));

        for event in ["check_run", "check_suite"] {
            let payload = json!({
                "action": "completed",
                event: { "head_sha": HEAD.to_uppercase(), "pull_requests": [{ "number": 3 }, { "number": 3 }, { "number": 0 }] },
                "repository": repo,
            });
            assert_eq!(
                parsed(event, &payload).targets,
                vec![
                    WebhookTarget::Change { number: 3 },
                    WebhookTarget::Head { sha: HEAD.into() }
                ],
                "{event}"
            );
        }
        let status = parsed(
            "status",
            &json!({ "sha": HEAD, "state": "success", "repository": repo }),
        );
        assert_eq!(
            status.targets,
            vec![WebhookTarget::Head { sha: HEAD.into() }]
        );
        let on_pr = parsed(
            "issue_comment",
            &json!({ "action": "created", "issue": { "number": 5, "pull_request": { "url": "x" } }, "repository": repo }),
        );
        assert_eq!(on_pr.targets, vec![WebhookTarget::Intake]);
        let on_issue = parsed(
            "issue_comment",
            &json!({ "action": "created", "issue": { "number": 6 }, "repository": repo }),
        );
        assert_eq!(on_issue.targets, vec![WebhookTarget::IssueIntake]);
        for action in ["opened", "labeled", "closed", "reopened", "edited"] {
            let issue = parsed(
                "issues",
                &json!({ "action": action, "issue": { "number": 6 }, "repository": repo }),
            );
            assert_eq!(issue.targets, vec![WebhookTarget::IssueIntake], "{action}");
        }
        let assigned = parsed(
            "issues",
            &json!({ "action": "assigned", "issue": { "number": 6 }, "repository": repo }),
        );
        assert!(assigned.targets.is_empty());
        for event in ["ping", "push", "issues"] {
            assert!(
                parsed(event, &json!({ "zen": "x", "repository": repo }))
                    .targets
                    .is_empty()
            );
        }
    }

    #[test]
    fn targets_are_capped() {
        let prs: Vec<Value> = (1..=150).map(|n| json!({ "number": n })).collect();
        let payload = json!({ "check_run": { "head_sha": HEAD, "pull_requests": prs } });
        assert_eq!(
            parsed("check_run", &payload).targets.len(),
            MAX_WEBHOOK_TARGETS
        );
    }

    #[test]
    fn a_bad_or_missing_signature_is_refused_before_the_body_is_read() {
        let body = b"not json at all";
        let codes: Vec<String> = [
            headers("ping", "sha256=00"),
            headers("ping", &sign(b"other body")),
            headers("ping", &sign(body).replace("sha256=", "sha1=")),
            vec![("X-GitHub-Event".into(), "ping".into())],
            vec![
                ("X-GitHub-Delivery".into(), "a".into()),
                ("X-Hub-Signature-256".into(), sign(body)),
            ],
        ]
        .iter()
        .map(|h| parse(SECRET, h, body).unwrap_err().code)
        .collect();
        assert!(
            codes.iter().all(|c| c == "webhook_signature_invalid"),
            "{codes:?}"
        );
        let err = parse(SECRET, &headers("ping", &sign(body)), body).unwrap_err();
        assert_eq!(err.code, "webhook_malformed");
        assert!(!err.retryable);
    }

    #[test]
    fn signature_hex_case_does_not_matter() {
        let body = br#"{"zen":"x"}"#;
        let upper = sign(body).to_uppercase().replace("SHA256=", "sha256=");
        assert!(parse(SECRET, &headers("ping", &upper), body).is_ok());
    }

    #[test]
    fn delivery_ids_outside_the_pattern_are_malformed() {
        let body = br#"{"zen":"x"}"#;
        for id in ["", "a b", "x/../y", &"a".repeat(65)] {
            let mut h = headers("ping", &sign(body));
            h[1].1 = id.to_string();
            let code = parse(SECRET, &h, body).unwrap_err().code;
            assert_eq!(code, "webhook_malformed", "{id:?}");
        }
    }
}
