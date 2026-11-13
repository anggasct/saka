use std::collections::BTreeMap;

use saka_ports::{
    BODY_DEDUP_WINDOW_SECS, DecodedDelivery, IncidentSeverity, IncidentSignal,
    MAX_SIGNALS_PER_DELIVERY, PortResult, SignalDecoder, SignalRequest, SignalStatus,
};
use serde::Deserialize;
use serde_json::Value;

use crate::auth::{
    body_digest, header, malformed, parse_json, refused, require_secret, tokens_equal,
};
use crate::normalize;

const BEARER: &str = "bearer ";
const PAYLOAD_VERSION: &str = "4";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlertmanagerOptions {
    pub environment_label: String,
    pub severity_label: String,
}

impl Default for AlertmanagerOptions {
    fn default() -> AlertmanagerOptions {
        AlertmanagerOptions {
            environment_label: "environment".to_string(),
            severity_label: "severity".to_string(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct AlertmanagerDecoder {
    options: AlertmanagerOptions,
}

impl AlertmanagerDecoder {
    pub fn new(options: AlertmanagerOptions) -> AlertmanagerDecoder {
        AlertmanagerDecoder { options }
    }
}

#[derive(Deserialize)]
struct Payload {
    version: String,
    alerts: Vec<Value>,
}

#[derive(Deserialize)]
struct Alert {
    status: String,
    fingerprint: String,
    #[serde(default)]
    labels: BTreeMap<String, String>,
    #[serde(default)]
    annotations: BTreeMap<String, String>,
    #[serde(default, rename = "startsAt")]
    starts_at: Option<String>,
    #[serde(default, rename = "generatorURL")]
    generator_url: Option<String>,
}

fn severity(label: Option<&String>) -> IncidentSeverity {
    let Some(label) = label else {
        return IncidentSeverity::Medium;
    };
    match label.to_ascii_lowercase().as_str() {
        "critical" | "page" => IncidentSeverity::Critical,
        "high" | "error" => IncidentSeverity::High,
        "warning" | "medium" => IncidentSeverity::Medium,
        "low" => IncidentSeverity::Low,
        _ => IncidentSeverity::Info,
    }
}

impl AlertmanagerDecoder {
    fn signal(&self, value: Value) -> PortResult<IncidentSignal> {
        let alert: Alert = serde_json::from_value(value).map_err(|_| malformed("alerts"))?;
        let fingerprint =
            normalize::fingerprint(&alert.fingerprint).ok_or_else(|| malformed("fingerprint"))?;
        let status = alert
            .status
            .parse::<SignalStatus>()
            .map_err(|_| malformed("status"))?;
        let title = [
            alert.annotations.get("summary"),
            alert.labels.get("alertname"),
        ]
        .into_iter()
        .flatten()
        .map(|raw| normalize::title(raw))
        .find(|title| !title.is_empty())
        .unwrap_or_else(|| normalize::title(&fingerprint));
        Ok(IncidentSignal {
            fingerprint,
            status,
            severity: severity(alert.labels.get(&self.options.severity_label)),
            title,
            summary: alert
                .annotations
                .get("description")
                .map(|raw| normalize::text(raw))
                .unwrap_or_default(),
            environment: alert
                .labels
                .get(&self.options.environment_label)
                .and_then(|raw| normalize::environment(raw)),
            url: alert.generator_url.as_deref().and_then(normalize::url),
            started_at: alert.starts_at.as_deref().and_then(normalize::time),
            labels: normalize::labels(
                alert
                    .labels
                    .iter()
                    .map(|(key, value)| (key.as_str(), value.as_str())),
            ),
        })
    }
}

impl SignalDecoder for AlertmanagerDecoder {
    fn decode(&self, request: &SignalRequest<'_>) -> PortResult<DecodedDelivery> {
        require_secret(request.secret)?;
        let given = header(request.headers, "authorization")
            .filter(|value| {
                value
                    .get(..BEARER.len())
                    .is_some_and(|scheme| scheme.eq_ignore_ascii_case(BEARER))
            })
            .and_then(|value| value.get(BEARER.len()..))
            .ok_or_else(refused)?;
        if !tokens_equal(given.trim().as_bytes(), request.secret) {
            return Err(refused());
        }
        let payload: Payload = parse_json(request.body)?;
        if payload.version != PAYLOAD_VERSION {
            return Err(malformed("version"));
        }
        let truncated = payload
            .alerts
            .len()
            .saturating_sub(MAX_SIGNALS_PER_DELIVERY);
        let signals = payload
            .alerts
            .into_iter()
            .take(MAX_SIGNALS_PER_DELIVERY)
            .map(|alert| self.signal(alert))
            .collect::<PortResult<Vec<_>>>()?;
        Ok(DecodedDelivery {
            delivery_key: body_digest(request.body),
            dedup_window_secs: Some(BODY_DEDUP_WINDOW_SECS),
            ignored: false,
            signals,
            truncated,
        })
    }
}
