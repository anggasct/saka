use std::fmt;

use crate::ids::{CredentialId, DeviceId, PairingId};
use crate::time::Timestamp;
use crate::vocab::{DeviceKind, PairingPurpose};

pub const NAME_MAX: usize = 100;
pub const MAX_FAILED_ATTEMPTS: i64 = 5;
pub const CODE_LEN: usize = 10;
pub const SECRET_BYTES: usize = 32;
pub const PAIRING_TTL_MS: i64 = 5 * 60 * 1000;
pub const SETUP_TTL_MS: i64 = 15 * 60 * 1000;
pub const DEVICE_TOKEN_PREFIX: &str = "sdt_";
pub const SESSION_TOKEN_PREFIX: &str = "sss_";
pub const TOUCH_INTERVAL_MS: i64 = 60_000;
pub const SESSION_HOURS_DEFAULT: u32 = 168;
pub const SESSION_HOURS_MAX: u32 = 720;

const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Actor {
    Local,
    Device(DeviceId),
    Passkey(CredentialId),
}

impl fmt::Display for Actor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Actor::Local => f.write_str("operator:local"),
            Actor::Device(id) => write!(f, "operator:device:{id}"),
            Actor::Passkey(id) => write!(f, "operator:passkey:{id}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub id: DeviceId,
    pub name: String,
    pub kind: DeviceKind,
    pub created_at: Timestamp,
    pub last_used_at: Option<Timestamp>,
    pub revoked_at: Option<Timestamp>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Credential {
    pub id: CredentialId,
    pub name: String,
    pub created_at: Timestamp,
    pub last_used_at: Option<Timestamp>,
    pub revoked_at: Option<Timestamp>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pairing {
    pub id: PairingId,
    pub purpose: PairingPurpose,
    pub created_by: String,
    pub created_at: Timestamp,
    pub expires_at: Timestamp,
}

impl PairingPurpose {
    pub fn ttl_ms(self) -> i64 {
        match self {
            PairingPurpose::Setup => SETUP_TTL_MS,
            PairingPurpose::Passkey | PairingPurpose::Token => PAIRING_TTL_MS,
        }
    }
}

pub fn name_valid(name: &str) -> bool {
    let chars = name.chars().count();
    name.trim() == name && (1..=NAME_MAX).contains(&chars) && !name.chars().any(char::is_control)
}

pub fn pairing_code(entropy: &[u8; CODE_LEN]) -> String {
    entropy
        .iter()
        .map(|b| char::from(ALPHABET[usize::from(b & 0x1f)]))
        .collect()
}

pub fn normalize_code(input: &str) -> Option<String> {
    let mut out = String::with_capacity(CODE_LEN);
    for c in input.chars() {
        let c = match c.to_ascii_uppercase() {
            '-' | ' ' => continue,
            'O' => '0',
            'I' | 'L' => '1',
            c => c,
        };
        if !c.is_ascii() || !ALPHABET.contains(&(c as u8)) {
            return None;
        }
        out.push(c);
    }
    (out.len() == CODE_LEN).then_some(out)
}

pub fn secret_token(prefix: &str, entropy: &[u8; SECRET_BYTES]) -> String {
    let mut out = String::with_capacity(prefix.len() + SECRET_BYTES * 2);
    out.push_str(prefix);
    for b in entropy {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

pub fn token_shape_valid(prefix: &str, token: &str) -> bool {
    token.strip_prefix(prefix).is_some_and(|rest| {
        rest.len() == SECRET_BYTES * 2 && rest.bytes().all(|b| b.is_ascii_hexdigit())
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scheme {
    Http,
    Https,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OriginParts {
    pub scheme: Scheme,
    pub host: String,
    pub port: Option<u16>,
}

impl OriginParts {
    pub fn effective_port(&self) -> u16 {
        self.port.unwrap_or(match self.scheme {
            Scheme::Http => 80,
            Scheme::Https => 443,
        })
    }

    pub fn serialized(&self) -> String {
        let scheme = match self.scheme {
            Scheme::Http => "http",
            Scheme::Https => "https",
        };
        match self.port {
            Some(port) => format!("{scheme}://{}:{port}", self.host),
            None => format!("{scheme}://{}", self.host),
        }
    }

    pub fn is_dev_localhost(&self) -> bool {
        self.scheme == Scheme::Http && self.host == "localhost"
    }
}

fn domain_label_ok(label: &str) -> bool {
    !label.is_empty()
        && label.len() <= 63
        && !label.starts_with('-')
        && !label.ends_with('-')
        && label
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

fn domain_ok(host: &str) -> bool {
    host.len() <= 253 && host.split('.').all(domain_label_ok)
}

fn is_ip_literal(host: &str) -> bool {
    host.parse::<std::net::IpAddr>().is_ok()
        || host.bytes().all(|b| b.is_ascii_digit() || b == b'.')
}

pub fn parse_origin(raw: &str) -> Option<OriginParts> {
    let raw = raw.trim().trim_end_matches('/');
    let (scheme, rest) = if let Some(rest) = raw.strip_prefix("https://") {
        (Scheme::Https, rest)
    } else {
        (Scheme::Http, raw.strip_prefix("http://")?)
    };
    let (host, port) = match rest.split_once(':') {
        Some((host, port)) => {
            if port.is_empty() || !port.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            (host, Some(port.parse::<u16>().ok().filter(|p| *p != 0)?))
        }
        None => (rest, None),
    };
    let host = host.to_ascii_lowercase();
    (domain_ok(&host) && !is_ip_literal(&host)).then_some(OriginParts { scheme, host, port })
}

pub fn rp_id_valid(origin: &OriginParts, rp_id: &str) -> bool {
    let rp_id = rp_id.trim();
    if rp_id != rp_id.to_ascii_lowercase() || !domain_ok(rp_id) || is_ip_literal(rp_id) {
        return false;
    }
    rp_id == origin.host || (rp_id.contains('.') && origin.host.ends_with(&format!(".{rp_id}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_ten_unambiguous_characters() {
        let code = pairing_code(&[0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);
        assert_eq!(code, "0123456789");
        assert_eq!(pairing_code(&[255; CODE_LEN]), "ZZZZZZZZZZ");
        assert!(code.chars().all(|c| !matches!(c, 'I' | 'L' | 'O' | 'U')));
    }

    #[test]
    fn typed_codes_ignore_case_spaces_dashes_and_lookalikes() {
        assert_eq!(normalize_code("abcde-fghjk").as_deref(), Some("ABCDEFGHJK"));
        assert_eq!(normalize_code("o1l1 0O0O0O").as_deref(), Some("0111000000"));
        assert_eq!(normalize_code("ABCDEFGHJ"), None);
        assert_eq!(normalize_code("ABCDEFGHJKM"), None);
        assert_eq!(normalize_code("ABCDEFGHJU"), None);
        assert_eq!(normalize_code("ABCDEFGHJ\u{e9}"), None);
    }

    #[test]
    fn tokens_carry_a_prefix_and_full_entropy() {
        let token = secret_token(DEVICE_TOKEN_PREFIX, &[0xab; SECRET_BYTES]);
        assert_eq!(token.len(), 4 + 64);
        assert!(token_shape_valid(DEVICE_TOKEN_PREFIX, &token));
        assert!(!token_shape_valid(SESSION_TOKEN_PREFIX, &token));
        assert!(!token_shape_valid(DEVICE_TOKEN_PREFIX, "sdt_short"));
    }

    #[test]
    fn names_are_one_to_one_hundred_printable_characters() {
        assert!(name_valid("Pixel 9"));
        assert!(name_valid(&"a".repeat(100)));
        assert!(!name_valid(""));
        assert!(!name_valid(&"a".repeat(101)));
        assert!(!name_valid(" padded "));
        assert!(!name_valid("a\nb"));
    }

    #[test]
    fn origins_are_plain_scheme_host_and_port() {
        let o = parse_origin("https://Saka.Example.com/").unwrap();
        assert_eq!(o.serialized(), "https://saka.example.com");
        assert_eq!(o.effective_port(), 443);
        assert_eq!(
            parse_origin("https://x.dev:8443").unwrap().effective_port(),
            8443
        );
        for bad in [
            "",
            "saka.example.com",
            "ftp://x.dev",
            "https://",
            "https://x.dev/path",
            "https://x.dev?x=1",
            "https://user@x.dev",
            "https://x.dev:0",
            "https://x.dev:99999",
            "https://127.0.0.1",
            "https://[::1]",
            "https://-x.dev",
            "https://x_y.dev",
        ] {
            assert!(parse_origin(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn the_relying_party_is_the_origin_domain_or_a_parent() {
        let o = parse_origin("https://saka.home.example.com").unwrap();
        for ok in ["saka.home.example.com", "home.example.com", "example.com"] {
            assert!(rp_id_valid(&o, ok), "{ok}");
        }
        for bad in [
            "com",
            "other.com",
            "ample.com",
            "evil-example.com",
            "Example.com",
            "127.0.0.1",
            "",
        ] {
            assert!(!rp_id_valid(&o, bad), "{bad}");
        }
        let dev = parse_origin("http://localhost:5173").unwrap();
        assert!(dev.is_dev_localhost());
        assert!(rp_id_valid(&dev, "localhost"));
    }

    #[test]
    fn actors_name_their_credential() {
        let id = DeviceId(uuid::Uuid::nil());
        assert_eq!(Actor::Local.to_string(), "operator:local");
        assert_eq!(
            Actor::Device(id).to_string(),
            "operator:device:00000000-0000-0000-0000-000000000000"
        );
    }
}
