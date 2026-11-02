use std::fmt;

use zeroize::Zeroizing;

use crate::vocab::SecretPurpose;

pub const VALUE_MIN_BYTES: usize = 8;
pub const VALUE_MAX_BYTES: usize = 65_536;
pub const PER_PROJECT_MAX: usize = 200;

// Wiped on drop, never printed by `Debug`, and deliberately not `Serialize`.
#[derive(Clone, PartialEq, Eq)]
pub struct SecretText(Zeroizing<String>);

impl SecretText {
    pub fn new(value: String) -> SecretText {
        SecretText(Zeroizing::new(value))
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretText(«redacted»)")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretVar {
    pub name: String,
    pub value: SecretText,
    pub purposes: Vec<SecretPurpose>,
}

impl SecretVar {
    pub fn serves(&self, purpose: SecretPurpose) -> bool {
        self.purposes.contains(&purpose)
    }
}

pub fn aad(project_id: uuid::Uuid, name: &str) -> Vec<u8> {
    let mut out = project_id.to_string().into_bytes();
    out.push(0);
    out.extend_from_slice(name.as_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_never_shows_the_value() {
        let v = SecretVar {
            name: "NPM_TOKEN".into(),
            value: SecretText::new("s3cr3t-value-123".into()),
            purposes: vec![SecretPurpose::Setup],
        };
        let text = format!("{v:?}");
        assert!(!text.contains("s3cr3t"), "{text}");
        assert!(v.serves(SecretPurpose::Setup) && !v.serves(SecretPurpose::Session));
    }

    #[test]
    fn aad_binds_project_and_name() {
        let id = uuid::Uuid::nil();
        assert_eq!(
            aad(id, "A"),
            b"00000000-0000-0000-0000-000000000000\0A".to_vec()
        );
    }
}
