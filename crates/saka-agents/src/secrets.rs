use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;

use saka_core::secret::SecretVar;
use saka_core::vocab::SecretPurpose;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

// Handed from the server to one runner through a 0600 file in the runtime dir, never through
// launch.json (which stays in the data dir); the runner removes it as soon as it has read it.
#[derive(Default, Serialize, Deserialize)]
pub struct SessionSecrets {
    pub env: BTreeMap<String, String>,
    pub redact: Vec<(String, String)>,
}

impl SessionSecrets {
    pub fn of(secrets: &[SecretVar]) -> SessionSecrets {
        SessionSecrets {
            env: secrets
                .iter()
                .filter(|s| s.serves(SecretPurpose::Session))
                .map(|s| (s.name.clone(), s.value.expose().to_string()))
                .collect(),
            redact: secrets
                .iter()
                .map(|s| (s.name.clone(), s.value.expose().to_string()))
                .collect(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.env.is_empty() && self.redact.is_empty()
    }

    pub fn write(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(dir)?;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
        let _ = std::fs::remove_file(path);
        let bytes = Zeroizing::new(serde_json::to_vec(self).map_err(std::io::Error::other)?);
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(&bytes)?;
        file.sync_all()
    }

    pub fn take(path: &Path) -> std::io::Result<SessionSecrets> {
        let mut bytes = Zeroizing::new(Vec::new());
        let read = std::fs::File::open(path).and_then(|mut f| f.read_to_end(&mut bytes));
        let _ = std::fs::remove_file(path);
        read?;
        serde_json::from_slice(&bytes).map_err(std::io::Error::other)
    }
}
