use std::collections::VecDeque;
use std::sync::Mutex;

use saka_core::incident::IncidentSignal;
use saka_ports::{DecodedDelivery, PortError, PortResult, RawDelivery, Signals};
use serde_json::json;

#[derive(Debug, Default)]
pub struct FakeSignals {
    queue: Mutex<VecDeque<PortResult<DecodedDelivery>>>,
    seen: Mutex<Vec<(String, Vec<u8>)>>,
}

impl FakeSignals {
    pub fn deliver(&self, delivery: DecodedDelivery) {
        self.queue.lock().unwrap().push_back(Ok(delivery));
    }

    pub fn refuse(&self, code: &str) {
        self.queue
            .lock()
            .unwrap()
            .push_back(Err(PortError::permanent(code, json!({}))));
    }

    pub fn received(&self) -> Vec<(String, Vec<u8>)> {
        self.seen.lock().unwrap().clone()
    }
}

pub fn delivery(key: &str, signals: Vec<IncidentSignal>) -> DecodedDelivery {
    DecodedDelivery {
        delivery_key: key.to_string(),
        dedup_window_secs: None,
        ignored: false,
        signals,
        truncated: 0,
    }
}

impl Signals for FakeSignals {
    fn decode(&self, source: &str, delivery: &RawDelivery<'_>) -> PortResult<DecodedDelivery> {
        self.seen
            .lock()
            .unwrap()
            .push((source.to_string(), delivery.body.to_vec()));
        self.queue.lock().unwrap().pop_front().unwrap_or_else(|| {
            Err(PortError::permanent(
                "malformed_payload",
                json!({ "field": "body" }),
            ))
        })
    }
}
