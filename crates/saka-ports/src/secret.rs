use saka_core::secret::{SecretScope, SecretText};

use crate::PortResult;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sealed {
    pub nonce: Vec<u8>,
    pub ciphertext: Vec<u8>,
}

pub trait SecretStore: Send + Sync {
    fn seal(&self, scope: &SecretScope<'_>, value: &SecretText) -> PortResult<Sealed>;

    fn open(&self, scope: &SecretScope<'_>, sealed: &Sealed) -> PortResult<SecretText>;

    // Keyed, so a stored request hash cannot be used to guess the value offline.
    fn fingerprint(&self, scope: &SecretScope<'_>, value: &SecretText) -> String;
}
