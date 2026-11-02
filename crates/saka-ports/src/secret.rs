use saka_core::ids::ProjectId;
use saka_core::secret::SecretText;

use crate::PortResult;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sealed {
    pub nonce: Vec<u8>,
    pub ciphertext: Vec<u8>,
}

pub trait SecretStore: Send + Sync {
    fn seal(&self, project: ProjectId, name: &str, value: &SecretText) -> PortResult<Sealed>;

    fn open(&self, project: ProjectId, name: &str, sealed: &Sealed) -> PortResult<SecretText>;

    // Keyed, so a stored request hash cannot be used to guess the value offline.
    fn fingerprint(&self, project: ProjectId, name: &str, value: &SecretText) -> String;
}
