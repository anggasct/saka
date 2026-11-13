use std::collections::BTreeMap;

use saka_core::time::Timestamp;
use saka_ports::{
    IncidentSeverity, MAX_FINGERPRINT_CHARS, MAX_LABEL_KEY_CHARS, MAX_LABEL_VALUE_CHARS,
    MAX_LABELS, MAX_SUMMARY_BYTES, MAX_TITLE_CHARS, MAX_URL_BYTES,
};

const MAX_ENVIRONMENT_CHARS: usize = 64;

fn is_hidden(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{feff}'
        )
}

fn clip_chars(text: &str, max_chars: usize) -> String {
    text.chars().take(max_chars).collect()
}

pub(crate) fn line(raw: &str, max_chars: usize) -> String {
    let spaced: String = raw
        .chars()
        .map(|c| if is_hidden(c) { ' ' } else { c })
        .collect();
    clip_chars(spaced.trim(), max_chars)
}

pub(crate) fn title(raw: &str) -> String {
    line(raw, MAX_TITLE_CHARS)
}

pub(crate) fn text(raw: &str) -> String {
    let mut kept = String::new();
    for c in raw.chars() {
        let allowed = matches!(c, '\n' | '\t') || !is_hidden(c);
        if !allowed {
            continue;
        }
        if kept.len() + c.len_utf8() > MAX_SUMMARY_BYTES {
            break;
        }
        kept.push(c);
    }
    kept.trim().to_string()
}

pub(crate) fn fingerprint(raw: &str) -> Option<String> {
    let cleaned = line(raw, MAX_FINGERPRINT_CHARS + 1);
    let valid = !cleaned.is_empty()
        && cleaned.chars().count() <= MAX_FINGERPRINT_CHARS
        && cleaned == raw.trim();
    valid.then_some(cleaned)
}

pub(crate) fn environment(raw: &str) -> Option<String> {
    let cleaned = line(raw, MAX_ENVIRONMENT_CHARS);
    (!cleaned.is_empty()).then_some(cleaned)
}

pub(crate) fn url(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let lowered = trimmed.to_ascii_lowercase();
    let web = lowered.starts_with("https://") || lowered.starts_with("http://");
    let clean = !trimmed.chars().any(|c| is_hidden(c) || c.is_whitespace());
    (web && clean && trimmed.len() <= MAX_URL_BYTES).then(|| trimmed.to_string())
}

pub(crate) fn time(raw: &str) -> Option<Timestamp> {
    let parsed: jiff::Timestamp = raw.trim().parse().ok()?;
    let millis = parsed.as_millisecond();
    (millis > 0).then_some(Timestamp(millis))
}

pub(crate) fn labels<'p>(
    pairs: impl Iterator<Item = (&'p str, &'p str)>,
) -> BTreeMap<String, String> {
    let mut kept = BTreeMap::new();
    for (key, value) in pairs {
        let key = line(key, MAX_LABEL_KEY_CHARS);
        if key.is_empty() {
            continue;
        }
        kept.entry(key)
            .or_insert_with(|| line(value, MAX_LABEL_VALUE_CHARS));
    }
    let overflow: Vec<String> = kept.keys().skip(MAX_LABELS).cloned().collect();
    for key in overflow {
        kept.remove(&key);
    }
    kept
}

pub(crate) fn level_severity(level: &str) -> IncidentSeverity {
    match level.to_ascii_lowercase().as_str() {
        "fatal" => IncidentSeverity::Critical,
        "error" => IncidentSeverity::High,
        "warning" => IncidentSeverity::Medium,
        "info" | "debug" => IncidentSeverity::Info,
        _ => IncidentSeverity::Medium,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_are_one_clean_line_within_the_limit() {
        assert_eq!(title("  a\nb\u{202e}c\t"), "a b c");
        assert_eq!(title(&"x".repeat(500)).chars().count(), MAX_TITLE_CHARS);
    }

    #[test]
    fn text_keeps_newlines_and_drops_control_characters_within_the_byte_limit() {
        assert_eq!(text("a\r\nb\u{0}c"), "a\nbc");
        assert!(text(&"é".repeat(5000)).len() <= MAX_SUMMARY_BYTES);
    }

    #[test]
    fn only_web_urls_survive() {
        assert_eq!(
            url("https://a.example/x"),
            Some("https://a.example/x".into())
        );
        assert_eq!(url("HTTP://a.example"), Some("HTTP://a.example".into()));
        assert_eq!(url("javascript:alert(1)"), None);
        assert_eq!(url("https://a.example/a b"), None);
    }

    #[test]
    fn fingerprints_must_be_clean_and_bounded() {
        assert_eq!(fingerprint("abc"), Some("abc".into()));
        assert_eq!(fingerprint(""), None);
        assert_eq!(fingerprint("a\nb"), None);
        assert_eq!(fingerprint(&"f".repeat(201)), None);
        assert!(fingerprint(&"f".repeat(200)).is_some());
    }

    #[test]
    fn labels_are_bounded_in_count_key_and_value() {
        let pairs: Vec<(String, String)> = (0..40)
            .map(|n| (format!("k{n:02}"), "v".repeat(400)))
            .collect();
        let kept = labels(pairs.iter().map(|(k, v)| (k.as_str(), v.as_str())));
        assert_eq!(kept.len(), MAX_LABELS);
        assert!(
            kept.values()
                .all(|v| v.chars().count() == MAX_LABEL_VALUE_CHARS)
        );
    }

    #[test]
    fn unset_alertmanager_start_times_are_dropped() {
        assert_eq!(time("0001-01-01T00:00:00Z"), None);
        assert_eq!(
            time("2026-11-13T10:00:00Z"),
            Some(Timestamp(1_794_564_000_000))
        );
    }
}
