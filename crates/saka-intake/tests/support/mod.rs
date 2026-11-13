#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    dead_code,
    reason = "test helpers shared by several test files"
)]

use hmac::{Hmac, KeyInit, Mac};
use saka_core::time::Timestamp;
use saka_ports::SignalRequest;
use sha2::Sha256;

pub const SECRET: &[u8] = b"s3cr3t-signal-token";
pub const NOW: Timestamp = Timestamp(1_794_564_000_000);

pub fn fixture(name: &str) -> Vec<u8> {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(path).unwrap()
}

pub fn headers(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect()
}

pub fn hmac_hex(secret: &[u8], parts: &[&[u8]]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).unwrap();
    for part in parts {
        mac.update(part);
    }
    mac.finalize()
        .into_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub fn request<'a>(headers: &'a [(String, String)], body: &'a [u8]) -> SignalRequest<'a> {
    SignalRequest {
        headers,
        body,
        secret: SECRET,
        now: NOW,
    }
}
