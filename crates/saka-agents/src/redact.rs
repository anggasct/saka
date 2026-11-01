//! Redaction of everything the CLI prints, before it is written, sent, or logged. Each
//! match becomes `«secret:<name>»`. It cannot be turned off.

use std::borrow::Cow;

use regex::Regex;
use serde_json::Value;

/// Known values shorter than this are not redacted (too likely to hit normal text).
pub const KNOWN_VALUE_MIN: usize = 8;

/// `(name, pattern, replacement)`, in the order they are applied.
const PATTERNS: &[(&str, &str, &str)] = &[
    (
        "private_key",
        r"-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z0-9 ]*PRIVATE KEY-----",
        "«secret:private_key»",
    ),
    (
        "github_pat",
        r"github_pat_[A-Za-z0-9_]{22,255}",
        "«secret:github_pat»",
    ),
    (
        "github_token",
        r"gh[pousr]_[A-Za-z0-9]{36,255}",
        "«secret:github_token»",
    ),
    (
        "anthropic_key",
        r"sk-ant-[A-Za-z0-9_-]{20,}",
        "«secret:anthropic_key»",
    ),
    (
        "openai_key",
        r"sk-(?:proj-)?[A-Za-z0-9_-]{20,}",
        "«secret:openai_key»",
    ),
    (
        "aws_access_key",
        r"AKIA[0-9A-Z]{16}",
        "«secret:aws_access_key»",
    ),
    (
        "jwt",
        r"eyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}",
        "«secret:jwt»",
    ),
    (
        "auth_header",
        r"(?i)(authorization:\s*(?:bearer|basic|token)\s+)[A-Za-z0-9._~+/=-]{8,}",
        "${1}«secret:auth_header»",
    ),
];

/// Names of the patterns, `known_value` first.
pub const PATTERN_NAMES: &[&str] = &[
    "known_value",
    "private_key",
    "github_token",
    "github_pat",
    "anthropic_key",
    "openai_key",
    "aws_access_key",
    "jwt",
    "auth_header",
];

pub struct Redactor {
    known: Vec<String>,
    patterns: Vec<(Regex, &'static str)>,
}

impl Redactor {
    pub fn new(known_values: &[String]) -> Redactor {
        let mut known: Vec<String> = known_values
            .iter()
            .filter(|v| v.chars().count() >= KNOWN_VALUE_MIN)
            .cloned()
            .collect();
        // Longest first, so a value that contains another is replaced whole.
        known.sort_by_key(|v| std::cmp::Reverse(v.len()));
        let patterns = PATTERNS
            .iter()
            .filter_map(|(_, pattern, replacement)| {
                Regex::new(pattern).ok().map(|re| (re, *replacement))
            })
            .collect();
        Redactor { known, patterns }
    }

    pub fn text<'a>(&self, input: &'a str) -> Cow<'a, str> {
        let mut out = Cow::Borrowed(input);
        for value in &self.known {
            if out.contains(value.as_str()) {
                out = Cow::Owned(out.replace(value.as_str(), "«secret:known_value»"));
            }
        }
        for (re, replacement) in &self.patterns {
            if re.is_match(&out) {
                out = Cow::Owned(re.replace_all(&out, *replacement).into_owned());
            }
        }
        out
    }

    /// Redacts every string (and object key) in `value`.
    pub fn value(&self, value: &mut Value) {
        match value {
            Value::String(s) => {
                if let Cow::Owned(r) = self.text(s) {
                    *s = r;
                }
            }
            Value::Array(items) => items.iter_mut().for_each(|v| self.value(v)),
            Value::Object(map) => {
                let entries: Vec<(String, Value)> = std::mem::take(map).into_iter().collect();
                for (k, mut v) in entries {
                    self.value(&mut v);
                    map.insert(self.text(&k).into_owned(), v);
                }
            }
            _ => {}
        }
    }
}

/// One sample per pattern, for tests that feed every pattern through the runner.
pub fn samples(known_value: &str) -> Vec<(&'static str, String)> {
    vec![
        ("known_value", known_value.to_string()),
        (
            "private_key",
            "-----BEGIN RSA PRIVATE KEY-----\\nMIIEowIBAAKCAQEA0Z3VS5JJcds3xfn\\n-----END RSA PRIVATE KEY-----".to_string(),
        ),
        ("github_token", format!("ghp_{}", "a1B2".repeat(9))),
        ("github_pat", format!("github_pat_{}", "Ab3_".repeat(6))),
        ("anthropic_key", format!("sk-ant-api03-{}", "x9Y_".repeat(6))),
        ("openai_key", format!("sk-proj-{}", "Qw3-".repeat(6))),
        ("aws_access_key", "AKIAIOSFODNN7EXAMPLE".to_string()),
        (
            "jwt",
            "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U".to_string(),
        ),
        ("auth_header", "Authorization: Bearer abcdefgh12345678".to_string()),
    ]
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn every_pattern_is_replaced_by_its_name() {
        let r = Redactor::new(&["s3cr3t-value-123".to_string(), "short".to_string()]);
        for (name, sample) in samples("s3cr3t-value-123") {
            let line = format!("before {sample} after");
            let out = r.text(&line);
            assert!(!out.contains(&sample), "{name} leaked: {out}");
            assert!(out.contains(&format!("«secret:{name}»")), "{name}: {out}");
        }
        assert_eq!(r.text("short stays"), "short stays");
        assert_eq!(
            r.text("authorization: token abcdefgh12345678"),
            "authorization: token «secret:auth_header»"
        );
        assert_eq!(r.text("nothing here"), Cow::Borrowed("nothing here"));
    }

    #[test]
    fn values_are_redacted_deeply() {
        let r = Redactor::new(&[]);
        let token = format!("ghp_{}", "a1B2".repeat(9));
        let mut v = json!({ "input": { "command": format!("curl -H 'x: {token}'"), "list": [token.clone()] } });
        r.value(&mut v);
        assert!(!v.to_string().contains(&token));
        assert_eq!(v["input"]["list"][0], "«secret:github_token»");
    }

    #[test]
    fn names_match_the_documented_table() {
        let mut documented: Vec<&str> = PATTERN_NAMES[1..].to_vec();
        let mut applied: Vec<&str> = PATTERNS.iter().map(|p| p.0).collect();
        documented.sort();
        applied.sort();
        assert_eq!(documented, applied);
    }
}
