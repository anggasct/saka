use std::sync::Mutex;
use std::sync::atomic::{AtomicI64, Ordering};

use saka_core::time::Timestamp;
use saka_ports::{Clock, Entropy, Ids, PortResult};
use uuid::Uuid;

#[derive(Debug)]
pub struct FakeClock(AtomicI64);

impl FakeClock {
    pub const START: Timestamp = Timestamp(1_767_225_600_000);

    pub fn new() -> FakeClock {
        FakeClock(AtomicI64::new(Self::START.0))
    }

    pub fn advance(&self, ms: i64) {
        self.0.fetch_add(ms, Ordering::SeqCst);
    }

    pub fn set(&self, at: Timestamp) {
        self.0.store(at.0, Ordering::SeqCst);
    }
}

impl Default for FakeClock {
    fn default() -> Self {
        FakeClock::new()
    }
}

impl Clock for FakeClock {
    fn now(&self) -> Timestamp {
        Timestamp(self.0.load(Ordering::SeqCst))
    }
}

#[derive(Debug, Default)]
pub struct FakeIds(Mutex<u128>);

impl Ids for FakeIds {
    fn new_id(&self) -> Uuid {
        let mut n = self.0.lock().unwrap();
        *n += 1;
        Uuid::from_u128(*n)
    }
}

#[derive(Debug, Default)]
pub struct FakeEntropy(Mutex<u64>);

impl Entropy for FakeEntropy {
    fn fill(&self, buf: &mut [u8]) -> PortResult<()> {
        let mut state = self.0.lock().unwrap();
        for byte in buf {
            *state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z = *state;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            *byte = ((z ^ (z >> 31)) >> 24) as u8;
        }
        Ok(())
    }
}
