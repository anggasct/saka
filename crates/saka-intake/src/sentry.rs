use saka_core::time::Timestamp;
use saka_ports::{
    BODY_DEDUP_WINDOW_SECS, DecodedDelivery, IncidentSignal, PortResult, SignalDecoder,
    SignalRequest, SignalStatus,
};
use serde_json::Value;

use crate::auth::{
    body_digest, header, hmac_matches, is_delivery_id, malformed, parse_json, refused,
    require_secret,
};
use crate::normalize;

#[derive(Debug, Clone, Copy, Default)]
pub struct SentryDecoder;

impl SentryDecoder {
    pub fn new() -> SentryDecoder {
        SentryDecoder
    }
}

fn text<'v>(value: &'v Value, key: &str) -> Option<&'v str> {
    value.get(key).and_then(Value::as_str)
}

fn identifier(value: &Value, key: &str) -> Option<String> {
    match value.get(key)? {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

fn time(value: &Value, key: &str) -> Option<Timestamp> {
    text(value, key).and_then(normalize::time)
}

fn labels(pairs: &[(&str, Option<&str>)]) -> std::collections::BTreeMap<String, String> {
    normalize::labels(
        pairs
            .iter()
            .filter_map(|(key, value)| value.map(|value| (*key, value))),
    )
}

fn issue_signal(payload: &Value) -> PortResult<Option<IncidentSignal>> {
    let status = match text(payload, "action") {
        Some("created" | "unresolved") => SignalStatus::Firing,
        Some("resolved") => SignalStatus::Resolved,
        _ => return Ok(None),
    };
    let issue = payload
        .get("data")
        .and_then(|data| data.get("issue"))
        .filter(|issue| issue.is_object())
        .ok_or_else(|| malformed("data.issue"))?;
    let fingerprint = identifier(issue, "id")
        .and_then(|id| normalize::fingerprint(&id))
        .ok_or_else(|| malformed("fingerprint"))?;
    let project = issue
        .get("project")
        .and_then(|project| text(project, "slug"));
    Ok(Some(IncidentSignal {
        severity: normalize::level_severity(text(issue, "level").unwrap_or_default()),
        title: normalize::title(text(issue, "title").unwrap_or_default()),
        summary: normalize::text(text(issue, "culprit").unwrap_or_default()),
        environment: None,
        url: text(issue, "permalink")
            .and_then(normalize::url)
            .or_else(|| text(issue, "web_url").and_then(normalize::url)),
        started_at: time(issue, "firstSeen"),
        labels: labels(&[("project", project)]),
        fingerprint,
        status,
    }))
}

fn event_alert_signal(payload: &Value) -> PortResult<IncidentSignal> {
    let event = payload
        .get("data")
        .and_then(|data| data.get("event"))
        .filter(|event| event.is_object())
        .ok_or_else(|| malformed("data.event"))?;
    let fingerprint = identifier(event, "issue_id")
        .or_else(|| identifier(event, "groupID"))
        .and_then(|id| normalize::fingerprint(&id))
        .ok_or_else(|| malformed("fingerprint"))?;
    let rule = payload
        .get("data")
        .and_then(|data| text(data, "triggered_rule"));
    Ok(IncidentSignal {
        severity: normalize::level_severity(text(event, "level").unwrap_or_default()),
        title: normalize::title(text(event, "title").unwrap_or_default()),
        summary: normalize::text(text(event, "culprit").unwrap_or_default()),
        environment: text(event, "environment").and_then(normalize::environment),
        url: text(event, "web_url").and_then(normalize::url),
        started_at: time(event, "datetime"),
        labels: labels(&[("rule", rule), ("platform", text(event, "platform"))]),
        fingerprint,
        status: SignalStatus::Firing,
    })
}

impl SignalDecoder for SentryDecoder {
    fn decode(&self, request: &SignalRequest<'_>) -> PortResult<DecodedDelivery> {
        require_secret(request.secret)?;
        let signature = header(request.headers, "sentry-hook-signature").ok_or_else(refused)?;
        if !hmac_matches(request.secret, &[request.body], signature.trim()) {
            return Err(refused());
        }
        let (delivery_key, dedup_window_secs) = match header(request.headers, "request-id")
            .map(str::trim)
            .filter(|id| is_delivery_id(id))
        {
            Some(id) => (id.to_string(), None),
            None => (body_digest(request.body), Some(BODY_DEDUP_WINDOW_SECS)),
        };
        let resource = header(request.headers, "sentry-hook-resource").map(str::trim);
        let ignored = |delivery_key: String| DecodedDelivery {
            delivery_key,
            dedup_window_secs,
            ignored: true,
            signals: Vec::new(),
            truncated: 0,
        };
        let signal = match resource {
            Some("issue") => issue_signal(&parse_json::<Value>(request.body)?)?,
            Some("event_alert") => Some(event_alert_signal(&parse_json::<Value>(request.body)?)?),
            _ => None,
        };
        Ok(match signal {
            Some(signal) => DecodedDelivery {
                delivery_key,
                dedup_window_secs,
                ignored: false,
                signals: vec![signal],
                truncated: 0,
            },
            None => ignored(delivery_key),
        })
    }
}
