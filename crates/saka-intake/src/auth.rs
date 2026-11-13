use hmac::{Hmac, KeyInit, Mac};
use saka_ports::{PortError, PortResult};
use serde::de::DeserializeOwned;
use serde_json::json;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

pub(crate) fn header<'h>(headers: &'h [(String, String)], name: &str) -> Option<&'h str> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

pub(crate) fn refused() -> PortError {
    PortError::permanent("webhook_signature_invalid", json!({}))
}

pub(crate) fn malformed(field: &str) -> PortError {
    PortError::permanent("malformed_payload", json!({ "field": field }))
}

pub(crate) fn require_secret(secret: &[u8]) -> PortResult<()> {
    if secret.is_empty() {
        Err(refused())
    } else {
        Ok(())
    }
}

pub(crate) fn tokens_equal(given: &[u8], expected: &[u8]) -> bool {
    let given = Sha256::digest(given);
    let expected = Sha256::digest(expected);
    bool::from(given.ct_eq(&expected))
}

pub(crate) fn hmac_matches(secret: &[u8], parts: &[&[u8]], signature_hex: &str) -> bool {
    let Some(signature) = decode_hex(signature_hex) else {
        return false;
    };
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(secret) else {
        return false;
    };
    for part in parts {
        mac.update(part);
    }
    mac.verify_slice(&signature).is_ok()
}

pub(crate) fn body_digest(body: &[u8]) -> String {
    Sha256::digest(body)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub(crate) fn parse_json<T: DeserializeOwned>(body: &[u8]) -> PortResult<T> {
    serde_json::from_slice(body).map_err(|_| malformed("body"))
}

pub(crate) fn is_delivery_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= saka_ports::MAX_DELIVERY_ID_CHARS
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn decode_hex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|at| {
            text.get(at..at + 2)
                .and_then(|pair| u8::from_str_radix(pair, 16).ok())
        })
        .collect()
}
