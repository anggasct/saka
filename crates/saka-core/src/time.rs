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

    pub fn to_rfc3339(self) -> String {
        let days = self.0.div_euclid(86_400_000);
        let ms_of_day = self.0.rem_euclid(86_400_000);
        let (year, month, day) = civil_from_days(days);
        format!(
            "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
            ms_of_day / 3_600_000,
            ms_of_day / 60_000 % 60,
            ms_of_day / 1000 % 60,
            ms_of_day % 1000
        )
    }
}

// Proleptic Gregorian calendar from days since 1970-01-01 (Howard Hinnant's `civil_from_days`).
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_format_as_utc_rfc3339_with_milliseconds() {
        assert_eq!(Timestamp(0).to_rfc3339(), "1970-01-01T00:00:00.000Z");
        assert_eq!(
            Timestamp(1_793_786_400_123).to_rfc3339(),
            "2026-11-04T10:00:00.123Z"
        );
        assert_eq!(
            Timestamp(951_782_400_000).to_rfc3339(),
            "2000-02-29T00:00:00.000Z"
        );
        assert_eq!(Timestamp(-1).to_rfc3339(), "1969-12-31T23:59:59.999Z");
    }
}
