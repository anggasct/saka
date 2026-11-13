use saka_core::time::Timestamp;

pub use saka_core::incident::IncidentSignal;
pub use saka_core::vocab::{IncidentSeverity, SignalStatus};

use crate::PortResult;

pub const MAX_SIGNALS_PER_DELIVERY: usize = 500;
pub const MAX_FINGERPRINT_CHARS: usize = 200;
pub const MAX_TITLE_CHARS: usize = 200;
pub const MAX_SUMMARY_BYTES: usize = 4096;
pub const MAX_URL_BYTES: usize = 2048;
pub const MAX_LABELS: usize = 32;
pub const MAX_LABEL_KEY_CHARS: usize = 64;
pub const MAX_LABEL_VALUE_CHARS: usize = 256;
pub const MAX_DELIVERY_ID_CHARS: usize = 128;
pub const TIMESTAMP_WINDOW_SECS: i64 = 300;
pub const BODY_DEDUP_WINDOW_SECS: u64 = 600;

#[derive(Debug, Clone, Copy)]
pub struct SignalRequest<'a> {
    pub headers: &'a [(String, String)],
    pub body: &'a [u8],
    pub secret: &'a [u8],
    pub now: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedDelivery {
    pub delivery_key: String,
    pub dedup_window_secs: Option<u64>,
    pub ignored: bool,
    pub signals: Vec<IncidentSignal>,
    pub truncated: usize,
}

pub trait SignalDecoder: Send + Sync {
    fn decode(&self, request: &SignalRequest<'_>) -> PortResult<DecodedDelivery>;
}

#[derive(Debug, Clone, Copy)]
pub struct RawDelivery<'a> {
    pub headers: &'a [(String, String)],
    pub body: &'a [u8],
    pub now: Timestamp,
}

pub trait Signals: Send + Sync {
    fn decode(&self, source: &str, delivery: &RawDelivery<'_>) -> PortResult<DecodedDelivery>;
}
