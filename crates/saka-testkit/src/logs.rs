use std::sync::Once;

use tracing::span::{Attributes, Id, Record};
use tracing::subscriber::Interest;
use tracing::{Event, Metadata, Subscriber};

struct EveryCallsite;

impl Subscriber for EveryCallsite {
    fn register_callsite(&self, _: &'static Metadata<'static>) -> Interest {
        Interest::sometimes()
    }
    fn enabled(&self, _: &Metadata<'_>) -> bool {
        false
    }
    fn new_span(&self, _: &Attributes<'_>) -> Id {
        Id::from_u64(1)
    }
    fn record(&self, _: &Id, _: &Record<'_>) {}
    fn record_follows_from(&self, _: &Id, _: &Id) {}
    fn event(&self, _: &Event<'_>) {}
    fn enter(&self, _: &Id) {}
    fn exit(&self, _: &Id) {}
}

static GLOBAL: Once = Once::new();

// A callsite first hit on another thread while a test subscriber registers can stay cached as
// disabled; a global subscriber that answers "sometimes" keeps every callsite checked against
// the subscriber of the thread that emits it.
pub fn check_callsites_per_thread() {
    GLOBAL.call_once(|| {
        let _ = tracing::subscriber::set_global_default(EveryCallsite);
    });
}
