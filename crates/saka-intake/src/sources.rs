use std::collections::BTreeMap;

use saka_core::secret::SecretText;
use saka_ports::{PortError, PortResult, RawDelivery, SignalDecoder, SignalRequest, Signals};
use serde_json::json;

struct Entry {
    decoder: Box<dyn SignalDecoder>,
    secret: SecretText,
}

#[derive(Default)]
pub struct IntakeSources {
    entries: BTreeMap<String, Entry>,
}

impl IntakeSources {
    pub fn new() -> IntakeSources {
        IntakeSources::default()
    }

    pub fn insert(&mut self, name: &str, decoder: Box<dyn SignalDecoder>, secret: SecretText) {
        self.entries
            .insert(name.to_string(), Entry { decoder, secret });
    }
}

impl Signals for IntakeSources {
    fn decode(
        &self,
        source: &str,
        delivery: &RawDelivery<'_>,
    ) -> PortResult<saka_ports::DecodedDelivery> {
        let entry = self.entries.get(source).ok_or_else(|| {
            PortError::permanent("incident_source_unknown", json!({ "source": source }))
        })?;
        entry.decoder.decode(&SignalRequest {
            headers: delivery.headers,
            body: delivery.body,
            secret: entry.secret.expose().as_bytes(),
            now: delivery.now,
        })
    }
}
