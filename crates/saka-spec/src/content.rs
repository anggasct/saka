use sha2::{Digest, Sha256};

use crate::frontmatter;

pub fn content_hash(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    format!("sha256:{hex}")
}

pub fn is_spec(text: &str) -> bool {
    frontmatter::read(text, "").is_some_and(|f| f.is_spec)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Locale, template};
    use saka_core::vocab::WorkKind;

    #[test]
    fn content_hashes_are_prefixed_sha256_of_the_raw_bytes() {
        assert_eq!(
            content_hash(b""),
            "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_ne!(content_hash(b"a\n"), content_hash(b"a\r\n"));
    }

    #[test]
    fn a_spec_is_a_file_whose_frontmatter_says_saka_spec() {
        for locale in Locale::ALL {
            assert!(is_spec(template(locale, WorkKind::Feature)));
        }
        assert!(is_spec("---\r\nsaka: spec\r\n---\r\n# T\r\n"));
        assert!(is_spec("---\nsaka: \"spec\"\n---\n"));
        assert!(!is_spec("# Notes\n"));
        assert!(!is_spec("---\ntitle: notes\n---\n"));
        assert!(!is_spec("---\nsaka: doc\n---\n"));
        assert!(!is_spec("\u{feff}---\nsaka: spec\n---\n"));
        assert!(!is_spec(""));
    }
}
