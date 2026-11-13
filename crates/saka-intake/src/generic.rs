use std::collections::BTreeMap;

use saka_core::time::Timestamp;
use saka_ports::{
    DecodedDelivery, IncidentSeverity, IncidentSignal, PortResult, SignalDecoder, SignalRequest,
    SignalStatus, TIMESTAMP_WINDOW_SECS,
};
use serde::Deserialize;

use crate::auth::{
    header, hmac_matches, is_delivery_id, malformed, parse_json, refused, require_secret,
};
use crate::normalize;

const SIGNATURE_PREFIX: &str = "sha256=";

#[derive(Debug, Clone, Copy, Default)]
pub struct GenericDecoder;

impl GenericDecoder {
    pub fn new() -> GenericDecoder {
        GenericDecoder
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Body {
    fingerprint: String,
    status: String,
    severity: String,
    title: String,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    environment: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    started_at: Option<String>,
    #[serde(default)]
    labels: BTreeMap<String, String>,
}

fn in_window(timestamp_secs: i64, now: Timestamp) -> bool {
    let now_secs = now.0.div_euclid(1000);
    now_secs.abs_diff(timestamp_secs) <= TIMESTAMP_WINDOW_SECS.unsigned_abs()
}

impl SignalDecoder for GenericDecoder {
    fn decode(&self, request: &SignalRequest<'_>) -> PortResult<DecodedDelivery> {
        require_secret(request.secret)?;
        let timestamp = header(request.headers, "x-saka-timestamp")
            .map(str::trim)
            .ok_or_else(refused)?;
        let delivery = header(request.headers, "x-saka-delivery")
            .map(str::trim)
            .filter(|id| is_delivery_id(id))
            .ok_or_else(refused)?;
        let signature = header(request.headers, "x-saka-signature-256")
            .and_then(|value| value.trim().strip_prefix(SIGNATURE_PREFIX))
            .ok_or_else(refused)?;
        let parts: [&[u8]; 3] = [timestamp.as_bytes(), b".", request.body];
        if !hmac_matches(request.secret, &parts, signature) {
            return Err(refused());
        }
        let timestamp_secs: i64 = timestamp.parse().map_err(|_| refused())?;
        if !in_window(timestamp_secs, request.now) {
            return Err(refused());
        }
        let body: Body = parse_json(request.body)?;
        let fingerprint =
            normalize::fingerprint(&body.fingerprint).ok_or_else(|| malformed("fingerprint"))?;
        let title = normalize::title(&body.title);
        if title.is_empty() {
            return Err(malformed("title"));
        }
        let started_at = match body.started_at.as_deref() {
            Some(raw) => Some(normalize::time(raw).ok_or_else(|| malformed("started_at"))?),
            None => None,
        };
        let signal = IncidentSignal {
            status: body
                .status
                .parse::<SignalStatus>()
                .map_err(|_| malformed("status"))?,
            severity: body
                .severity
                .parse::<IncidentSeverity>()
                .map_err(|_| malformed("severity"))?,
            summary: body
                .summary
                .as_deref()
                .map(normalize::text)
                .unwrap_or_default(),
            environment: body.environment.as_deref().and_then(normalize::environment),
            url: body.url.as_deref().and_then(normalize::url),
            labels: normalize::labels(
                body.labels
                    .iter()
                    .map(|(key, value)| (key.as_str(), value.as_str())),
            ),
            fingerprint,
            title,
            started_at,
        };
        Ok(DecodedDelivery {
            delivery_key: delivery.to_string(),
            dedup_window_secs: None,
            ignored: false,
            signals: vec![signal],
            truncated: 0,
        })
    }
}
