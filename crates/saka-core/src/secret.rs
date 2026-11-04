use std::fmt;

use zeroize::Zeroizing;

use crate::ids::{ProjectId, UserLinkAuthorizationId, UserLinkId};
use crate::vocab::{SecretPurpose, UserTokenKind};

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

// Each scope binds a ciphertext to the row it was sealed for, so a value copied to another row,
// or read as another kind of credential, fails to open. Project AADs start with a hex UUID and
// every other prefix starts with a non-hex letter, so two scopes never share an AAD.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretScope<'a> {
    Project {
        project_id: ProjectId,
        name: &'a str,
    },
    UserLinkToken {
        link_id: UserLinkId,
        kind: UserTokenKind,
    },
    DeviceCode {
        authorization_id: UserLinkAuthorizationId,
    },
    ProjectUserToken {
        project_id: ProjectId,
    },
}

impl SecretScope<'_> {
    pub fn aad(&self) -> Vec<u8> {
        let mut out = Vec::new();
        let mut part = |bytes: &[u8]| {
            if !out.is_empty() {
                out.push(0);
            }
            out.extend_from_slice(bytes);
        };
        match self {
            SecretScope::Project { project_id, name } => {
                part(project_id.to_string().as_bytes());
                part(name.as_bytes());
            }
            SecretScope::UserLinkToken { link_id, kind } => {
                part(b"user_link");
                part(link_id.to_string().as_bytes());
                part(kind.as_str().as_bytes());
            }
            SecretScope::DeviceCode { authorization_id } => {
                part(b"user_link_authorization");
                part(authorization_id.to_string().as_bytes());
            }
            SecretScope::ProjectUserToken { project_id } => {
                part(b"project_user_token");
                part(project_id.to_string().as_bytes());
            }
        }
        out
    }

    pub fn label(&self) -> &str {
        match self {
            SecretScope::Project { name, .. } => name,
            SecretScope::UserLinkToken {
                kind: UserTokenKind::Access,
                ..
            } => "user_link_access",
            SecretScope::UserLinkToken {
                kind: UserTokenKind::Refresh,
                ..
            } => "user_link_refresh",
            SecretScope::DeviceCode { .. } => "device_code",
            SecretScope::ProjectUserToken { .. } => "project_user_token",
        }
    }
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
        let scope = SecretScope::Project {
            project_id: ProjectId(uuid::Uuid::nil()),
            name: "A",
        };
        assert_eq!(
            scope.aad(),
            b"00000000-0000-0000-0000-000000000000\0A".to_vec()
        );
    }

    #[test]
    fn every_credential_kind_has_its_own_aad() {
        let id = uuid::Uuid::from_u128(7);
        let hex = id.to_string();
        let scopes = [
            SecretScope::Project {
                project_id: ProjectId(id),
                name: "user_link",
            },
            SecretScope::UserLinkToken {
                link_id: UserLinkId(id),
                kind: UserTokenKind::Access,
            },
            SecretScope::UserLinkToken {
                link_id: UserLinkId(id),
                kind: UserTokenKind::Refresh,
            },
            SecretScope::DeviceCode {
                authorization_id: UserLinkAuthorizationId(id),
            },
            SecretScope::ProjectUserToken {
                project_id: ProjectId(id),
            },
        ];
        let aads: Vec<Vec<u8>> = scopes.iter().map(SecretScope::aad).collect();
        assert_eq!(aads[1], format!("user_link\0{hex}\0access").into_bytes());
        assert_eq!(
            aads[3],
            format!("user_link_authorization\0{hex}").into_bytes()
        );
        assert_eq!(aads[4], format!("project_user_token\0{hex}").into_bytes());
        for (i, a) in aads.iter().enumerate() {
            for b in &aads[i + 1..] {
                assert_ne!(a, b);
            }
        }
    }
}
