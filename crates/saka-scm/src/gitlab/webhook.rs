use std::collections::BTreeSet;

use saka_ports::{PortError, PortResult, WebhookDelivery, WebhookTarget};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

const MAX_DELIVERY_ID: usize = 64;
const MAX_EVENT: usize = 64;
const DERIVED_ID_HEX: usize = 32;
const ID_HEADERS: [&str; 3] = ["webhook-id", "idempotency-key", "x-gitlab-event-uuid"];

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

pub(crate) fn verify(secret: &[u8], headers: &[(String, String)]) -> PortResult<()> {
    let given = header(headers, "x-gitlab-token").ok_or_else(refused)?;
    if bool::from(given.as_bytes().ct_eq(secret)) {
        Ok(())
    } else {
        Err(refused())
    }
}

fn delivery_id(headers: &[(String, String)], body: &[u8]) -> PortResult<String> {
    let id = match ID_HEADERS.iter().find_map(|name| header(headers, name)) {
        Some(id) => id.to_string(),
        None => Sha256::digest(body)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
            .chars()
            .take(DERIVED_ID_HEX)
            .collect(),
    };
    let valid = !id.is_empty()
        && id.len() <= MAX_DELIVERY_ID
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
    if valid {
        Ok(id)
    } else {
        Err(malformed("delivery_id"))
    }
}

fn is_sha(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

fn change(iid: &Value) -> Option<WebhookTarget> {
    iid.as_u64()
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

fn targets(kind: &str, payload: &Value) -> Vec<WebhookTarget> {
    let mut found: BTreeSet<WebhookTarget> = BTreeSet::new();
    match kind {
        "merge_request" => {
            let attrs = &payload["object_attributes"];
            found.extend(change(&attrs["iid"]));
            found.extend(head(&attrs["last_commit"]["id"]));
            found.insert(WebhookTarget::Intake);
        }
        "note" => match payload["object_attributes"]["noteable_type"].as_str() {
            Some("MergeRequest") => {
                found.insert(WebhookTarget::Intake);
            }
            Some("Issue") => {
                found.insert(WebhookTarget::IssueIntake);
            }
            _ => {}
        },
        "issue" => {
            found.insert(WebhookTarget::IssueIntake);
        }
        "pipeline" => {
            found.extend(head(&payload["object_attributes"]["sha"]));
            found.extend(change(&payload["merge_request"]["iid"]));
        }
        "build" => {
            found.extend(head(&payload["sha"]));
        }
        _ => {}
    }
    found.into_iter().collect()
}

fn action(kind: &str, payload: &Value) -> Option<String> {
    let value = match kind {
        "merge_request" => &payload["object_attributes"]["action"],
        "pipeline" => &payload["object_attributes"]["status"],
        "build" => &payload["build_status"],
        _ => &Value::Null,
    };
    value.as_str().map(String::from)
}

pub(crate) fn parse(
    secret: &[u8],
    headers: &[(String, String)],
    body: &[u8],
) -> PortResult<WebhookDelivery> {
    verify(secret, headers)?;
    let delivery_id = delivery_id(headers, body)?;
    let payload: Value = serde_json::from_slice(body).map_err(|_| malformed("body"))?;
    let kind = payload["object_kind"].as_str().unwrap_or_default();
    if kind.is_empty() || kind.len() > MAX_EVENT {
        return Err(malformed("event"));
    }
    Ok(WebhookDelivery {
        delivery_id,
        event: kind.to_string(),
        action: action(kind, &payload),
        repo_full_name: payload["project"]["path_with_namespace"]
            .as_str()
            .map(String::from),
        targets: targets(kind, &payload),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &[u8] = b"0123456789abcdef0123456789abcdef";
    const HEAD: &str = "abcdef0123456789abcdef0123456789abcdef01";

    fn headers() -> Vec<(String, String)> {
        vec![
            (
                "X-Gitlab-Token".into(),
                "0123456789abcdef0123456789abcdef".into(),
            ),
            (
                "webhook-id".into(),
                "f5e5f430-f57b-4e6e-9fac-d9128cd7232f".into(),
            ),
        ]
    }

    fn parsed(payload: &Value) -> WebhookDelivery {
        parse(SECRET, &headers(), &serde_json::to_vec(payload).unwrap()).unwrap()
    }

    #[test]
    fn each_event_names_its_targets() {
        let project = json!({ "path_with_namespace": "group/sub/app" });
        let mr = parsed(&json!({
            "object_kind": "merge_request", "project": project,
            "object_attributes": { "iid": 12, "action": "update", "last_commit": { "id": HEAD.to_uppercase() } },
        }));
        assert_eq!(
            mr.targets,
            vec![
                WebhookTarget::Change { number: 12 },
                WebhookTarget::Head { sha: HEAD.into() },
                WebhookTarget::Intake
            ]
        );
        assert_eq!(mr.action.as_deref(), Some("update"));
        assert_eq!(mr.repo_full_name.as_deref(), Some("group/sub/app"));
        assert_eq!(mr.event, "merge_request");

        let pipeline = parsed(&json!({
            "object_kind": "pipeline", "project": project,
            "object_attributes": { "sha": HEAD, "status": "success" },
            "merge_request": { "iid": 3 },
        }));
        assert_eq!(
            pipeline.targets,
            vec![
                WebhookTarget::Change { number: 3 },
                WebhookTarget::Head { sha: HEAD.into() }
            ]
        );
        assert_eq!(pipeline.action.as_deref(), Some("success"));

        let branch_pipeline = parsed(&json!({
            "object_kind": "pipeline", "object_attributes": { "sha": HEAD }, "merge_request": null,
        }));
        assert_eq!(
            branch_pipeline.targets,
            vec![WebhookTarget::Head { sha: HEAD.into() }]
        );
        assert_eq!(branch_pipeline.repo_full_name, None);

        let job = parsed(&json!({ "object_kind": "build", "sha": HEAD, "build_status": "failed" }));
        assert_eq!(job.targets, vec![WebhookTarget::Head { sha: HEAD.into() }]);
        assert_eq!(job.action.as_deref(), Some("failed"));

        let on_mr = parsed(&json!({
            "object_kind": "note", "object_attributes": { "noteable_type": "MergeRequest" },
        }));
        assert_eq!(on_mr.targets, vec![WebhookTarget::Intake]);
        let on_issue = parsed(&json!({
            "object_kind": "note", "object_attributes": { "noteable_type": "Issue" },
        }));
        assert_eq!(on_issue.targets, vec![WebhookTarget::IssueIntake]);
        let issue = parsed(&json!({
            "object_kind": "issue", "object_attributes": { "iid": 6, "action": "close" },
        }));
        assert_eq!(issue.targets, vec![WebhookTarget::IssueIntake]);
        for kind in ["push", "note"] {
            assert!(parsed(&json!({ "object_kind": kind })).targets.is_empty());
        }
    }

    #[test]
    fn the_token_must_match_exactly() {
        let body = serde_json::to_vec(&json!({ "object_kind": "push" })).unwrap();
        let with = |token: Option<&str>| {
            let mut h = headers();
            h.retain(|(k, _)| k != "X-Gitlab-Token");
            if let Some(t) = token {
                h.push(("x-gitlab-token".into(), t.into()));
            }
            parse(SECRET, &h, &body)
        };
        assert!(with(Some("0123456789abcdef0123456789abcdef")).is_ok());
        for bad in [
            None,
            Some(""),
            Some("0123456789abcdef0123456789abcdeF"),
            Some("0123456789abcdef0123456789abcde"),
            Some("0123456789abcdef0123456789abcdef0"),
        ] {
            assert_eq!(
                with(bad).unwrap_err().code,
                "webhook_signature_invalid",
                "{bad:?}"
            );
        }
    }

    #[test]
    fn the_delivery_id_falls_back_through_the_headers_to_a_body_hash() {
        let body = serde_json::to_vec(&json!({ "object_kind": "push" })).unwrap();
        let id_with = |extra: &[(&str, &str)]| {
            let mut h = vec![(
                "X-Gitlab-Token".to_string(),
                String::from_utf8(SECRET.to_vec()).unwrap(),
            )];
            h.extend(extra.iter().map(|(k, v)| (k.to_string(), v.to_string())));
            parse(SECRET, &h, &body).map(|d| d.delivery_id)
        };
        assert_eq!(
            id_with(&[
                ("Idempotency-Key", "b"),
                ("webhook-id", "a"),
                ("X-Gitlab-Event-UUID", "c")
            ])
            .unwrap(),
            "a"
        );
        assert_eq!(
            id_with(&[("Idempotency-Key", "b"), ("X-Gitlab-Event-UUID", "c")]).unwrap(),
            "b"
        );
        assert_eq!(id_with(&[("X-Gitlab-Event-UUID", "c")]).unwrap(), "c");
        let derived = id_with(&[]).unwrap();
        assert_eq!(derived.len(), 32);
        assert_eq!(derived, id_with(&[]).unwrap());
        for bad in ["", "has space", &"x".repeat(65)] {
            assert_eq!(
                id_with(&[("webhook-id", bad)]).unwrap_err().params["field"],
                "delivery_id",
                "{bad:?}"
            );
        }
    }

    #[test]
    fn a_body_that_is_not_an_event_is_malformed() {
        assert_eq!(
            parse(SECRET, &headers(), b"not json").unwrap_err().params["field"],
            "body"
        );
        assert_eq!(
            parse(SECRET, &headers(), b"{}").unwrap_err().params["field"],
            "event"
        );
    }
}
