use serde::{Deserialize, Serialize};

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct Timestamp(pub i64);

impl Timestamp {
    pub fn plus_ms(self, ms: i64) -> Timestamp {
        Timestamp(self.0.saturating_add(ms))
    }

    pub fn since(self, earlier: Timestamp) -> i64 {
        self.0.saturating_sub(earlier.0).max(0)
    }
}
