use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;

use chacha20poly1305::aead::{Aead, Generate, Key, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use saka_core::secret::{SecretScope, SecretText};
use saka_ports::{PortError, PortResult, Sealed, SecretStore};
use serde_json::json;
use sha2::Sha256;
use zeroize::{Zeroize, Zeroizing};

pub const KEY_BYTES: usize = 32;
const FINGERPRINT_INFO: &[u8] = b"saka secret request fingerprint v1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyFileError {
    pub code: &'static str,
    pub detail: String,
}

fn key_error(code: &'static str, path: &Path, detail: impl std::fmt::Display) -> KeyFileError {
    KeyFileError {
        code,
        detail: format!("{}: {detail}", path.display()),
    }
}

pub struct AeadSecrets {
    cipher: XChaCha20Poly1305,
    fingerprint_key: Zeroizing<[u8; 32]>,
}

impl AeadSecrets {
    pub fn from_key(key: &[u8; KEY_BYTES]) -> AeadSecrets {
        let mut fingerprint_key = Zeroizing::new([0u8; 32]);
        // A separate subkey: the AEAD key itself never doubles as a MAC key.
        let derived =
            Hkdf::<Sha256>::new(None, key).expand(FINGERPRINT_INFO, &mut *fingerprint_key);
        debug_assert!(derived.is_ok());
        AeadSecrets {
            cipher: XChaCha20Poly1305::new(&Key::<XChaCha20Poly1305>::from(*key)),
            fingerprint_key,
        }
    }

    pub fn from_key_file(path: &Path) -> Result<AeadSecrets, KeyFileError> {
        match std::fs::symlink_metadata(path) {
            Ok(meta) => {
                if !meta.file_type().is_file() {
                    return Err(key_error(
                        "config_key_file_invalid",
                        path,
                        "not a regular file",
                    ));
                }
                let mode = meta.permissions().mode() & 0o777;
                if mode != 0o600 || meta.uid() != rustix::process::getuid().as_raw() {
                    return Err(key_error(
                        "config_key_file_mode",
                        path,
                        format!("mode {mode:o}; want 600 and owned by this user"),
                    ));
                }
                let mut bytes = Zeroizing::new(Vec::new());
                std::fs::File::open(path)
                    .and_then(|mut f| f.read_to_end(&mut bytes))
                    .map_err(|e| key_error("config_key_file_unreadable", path, e))?;
                let key: &[u8; KEY_BYTES] = bytes.as_slice().try_into().map_err(|_| {
                    key_error(
                        "config_key_file_invalid",
                        path,
                        format!("{} bytes; want {KEY_BYTES}", bytes.len()),
                    )
                })?;
                Ok(AeadSecrets::from_key(key))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::create(path),
            Err(e) => Err(key_error("config_key_file_unreadable", path, e)),
        }
    }

    fn create(path: &Path) -> Result<AeadSecrets, KeyFileError> {
        let unreadable = |e: std::io::Error| key_error("config_key_file_unreadable", path, e);
        if let Some(dir) = path.parent() {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(dir)
                .map_err(unreadable)?;
        }
        let mut generated = Key::<XChaCha20Poly1305>::try_generate()
            .map_err(|e| key_error("config_key_file_unreadable", path, e))?;
        let mut key = Zeroizing::new([0u8; KEY_BYTES]);
        key.copy_from_slice(&generated);
        generated.as_mut_slice().zeroize();
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .map_err(unreadable)?;
        file.write_all(&*key)
            .and_then(|()| file.sync_all())
            .map_err(unreadable)?;
        // The umask can only narrow 0600; this makes the mode exact either way.
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(unreadable)?;
        Ok(AeadSecrets::from_key(&key))
    }
}

fn unreadable(scope: &SecretScope<'_>) -> PortError {
    PortError::permanent("secret_unreadable", json!({ "name": scope.label() }))
}

impl SecretStore for AeadSecrets {
    fn seal(&self, scope: &SecretScope<'_>, value: &SecretText) -> PortResult<Sealed> {
        let nonce = XNonce::try_generate().map_err(|e| {
            PortError::retryable("secret_seal_failed", json!({ "detail": e.to_string() }))
        })?;
        let aad = scope.aad();
        let ciphertext = self
            .cipher
            .encrypt(
                &nonce,
                Payload {
                    msg: value.expose().as_bytes(),
                    aad: &aad,
                },
            )
            .map_err(|_| {
                PortError::permanent("secret_seal_failed", json!({ "name": scope.label() }))
            })?;
        Ok(Sealed {
            nonce: nonce.to_vec(),
            ciphertext,
        })
    }

    fn open(&self, scope: &SecretScope<'_>, sealed: &Sealed) -> PortResult<SecretText> {
        let nonce = XNonce::try_from(sealed.nonce.as_slice()).map_err(|_| unreadable(scope))?;
        let aad = scope.aad();
        let plain = Zeroizing::new(
            self.cipher
                .decrypt(
                    &nonce,
                    Payload {
                        msg: &sealed.ciphertext,
                        aad: &aad,
                    },
                )
                .map_err(|_| unreadable(scope))?,
        );
        let text = std::str::from_utf8(&plain).map_err(|_| unreadable(scope))?;
        Ok(SecretText::new(text.to_string()))
    }

    fn fingerprint(&self, scope: &SecretScope<'_>, value: &SecretText) -> String {
        let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(&*self.fingerprint_key) else {
            return String::new();
        };
        mac.update(&scope.aad());
        mac.update(&[0]);
        mac.update(value.expose().as_bytes());
        mac.finalize()
            .into_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_key_file_is_created_0600_and_reopens() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("saka").join("secret.key");
        let a = AeadSecrets::from_key_file(&path).unwrap();
        let meta = std::fs::metadata(&path).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        assert_eq!(meta.len(), KEY_BYTES as u64);
        let b = AeadSecrets::from_key_file(&path).unwrap();
        let scope = SecretScope::Project {
            project_id: saka_core::ids::ProjectId(uuid::Uuid::from_u128(1)),
            name: "NPM_TOKEN",
        };
        let value = SecretText::new("npm-token-abcdef".into());
        let sealed = a.seal(&scope, &value).unwrap();
        assert_eq!(b.open(&scope, &sealed).unwrap(), value);
        assert_eq!(a.fingerprint(&scope, &value), b.fingerprint(&scope, &value));
    }
}
