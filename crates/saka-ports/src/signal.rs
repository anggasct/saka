use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use saka_core::time::Timestamp;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SignalSeverity {
    Info,
    Low,
    Medium,
    High,
    Critical,
}

impl SignalSeverity {
    pub const ALL: &'static [SignalSeverity] = &[
        SignalSeverity::Info,
        SignalSeverity::Low,
        SignalSeverity::Medium,
        SignalSeverity::High,
        SignalSeverity::Critical,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            SignalSeverity::Info => "info",
            SignalSeverity::Low => "low",
            SignalSeverity::Medium => "medium",
            SignalSeverity::High => "high",
            SignalSeverity::Critical => "critical",
        }
    }
}

impl fmt::Display for SignalSeverity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for SignalSeverity {
    type Err = ();

    fn from_str(value: &str) -> Result<SignalSeverity, ()> {
        SignalSeverity::ALL
            .iter()
            .copied()
            .find(|severity| severity.as_str() == value)
            .ok_or(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SignalStatus {
    Firing,
    Resolved,
}

impl SignalStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            SignalStatus::Firing => "firing",
            SignalStatus::Resolved => "resolved",
        }
    }
}

impl fmt::Display for SignalStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for SignalStatus {
    type Err = ();

    fn from_str(value: &str) -> Result<SignalStatus, ()> {
        match value {
            "firing" => Ok(SignalStatus::Firing),
            "resolved" => Ok(SignalStatus::Resolved),
            _ => Err(()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncidentSignal {
    pub fingerprint: String,
    pub status: SignalStatus,
    pub severity: SignalSeverity,
    pub title: String,
    pub summary: String,
    pub environment: Option<String>,
    pub url: Option<String>,
    pub started_at: Option<Timestamp>,
    pub labels: BTreeMap<String, String>,
}

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
