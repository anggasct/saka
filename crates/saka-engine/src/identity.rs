use saka_core::event::Event;
use saka_core::identity::{
    Actor, Credential, DEVICE_TOKEN_PREFIX, Device, Pairing, SECRET_BYTES, TOUCH_INTERVAL_MS,
    name_valid, normalize_code, pairing_code, secret_token, token_shape_valid,
};
use saka_core::ids::{DeviceId, PairingId};
use saka_core::rejection::Rejection;
use saka_core::secret::SecretText;
use saka_core::vocab::{DeviceKind, PairingPurpose};
use saka_store::Batch;
use saka_store::identity::{self as rows, NewDevice, NewPairing};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::action::{Action, Produced};
use crate::{Cause, Engine, EngineError, Result};

const LOCAL_CLI_NAME: &str = "Local CLI";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Principal {
    pub actor: Actor,
    pub device_kind: Option<DeviceKind>,
}

#[derive(Debug, Clone)]
pub struct PairingIssued {
    pub pairing: Pairing,
    pub code: SecretText,
}

#[derive(Debug, Clone)]
pub struct DeviceIssued {
    pub device: Device,
    pub token: SecretText,
}

pub fn digest(secret: &str) -> String {
    Sha256::digest(secret.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

impl Engine {
    fn random<const N: usize>(&self) -> Result<[u8; N]> {
        let mut bytes = [0u8; N];
        self.ports
            .entropy
            .fill(&mut bytes)
            .map_err(|e| EngineError::Internal(format!("entropy: {}", e.code)))?;
        Ok(bytes)
    }

    fn device_token(&self) -> Result<SecretText> {
        let entropy = self.random::<SECRET_BYTES>()?;
        Ok(SecretText::new(secret_token(DEVICE_TOKEN_PREFIX, &entropy)))
    }

    pub fn authenticate_token(&self, token: &str) -> Result<Option<Principal>> {
        if !token_shape_valid(DEVICE_TOKEN_PREFIX, token) {
            return Ok(None);
        }
        let hash = digest(token);
        let Some(device) = self.store.read(|c| rows::device_by_token(c, &hash))? else {
            return Ok(None);
        };
        let now = self.now();
        let stale = device
            .last_used_at
            .is_none_or(|t| now.0.saturating_sub(t.0) >= TOUCH_INTERVAL_MS);
        if stale {
            self.store
                .transact(|tx| rows::touch_device(tx, device.id, now))?;
        }
        Ok(Some(Principal {
            actor: Actor::Device(device.id),
            device_kind: Some(device.kind),
        }))
    }

    pub fn issue_local_cli_token(&self) -> Result<SecretText> {
        let _guard = self.locked()?;
        let token = self.device_token()?;
        let hash = digest(token.expose());
        let now = self.now();
        let reissued = self
            .store
            .transact(|tx| rows::reissue_local_cli(tx, &hash, now))?;
        if reissued.is_some() {
            return Ok(token);
        }
        let id = DeviceId(self.new_id());
        let device = Device {
            id,
            name: LOCAL_CLI_NAME.to_string(),
            kind: DeviceKind::LocalCli,
            created_at: now,
            last_used_at: None,
            revoked_at: None,
        };
        let cause = self.cause(&Actor::Device(id).to_string());
        let mut batch = Batch::new(now);
        let event = Event::DevicePaired {
            device_id: id,
            name: device.name.clone(),
            kind: device.kind,
            pairing_id: None,
        };
        self.push_event(&mut batch, &event, None, &cause, now);
        batch.new_devices.push(NewDevice {
            device,
            token_sha256: hash,
        });
        self.commit(&batch)?;
        Ok(token)
    }

    pub fn create_pairing(
        &self,
        purpose: PairingPurpose,
        cause: &Cause,
        action: Option<&Action<'_>>,
    ) -> Result<PairingIssued> {
        let _guard = self.locked()?;
        let now = self.now();
        let code = SecretText::new(pairing_code(&self.random()?));
        let pairing = Pairing {
            id: PairingId(self.new_id()),
            purpose,
            created_by: cause.actor.clone(),
            created_at: now,
            expires_at: saka_core::time::Timestamp(now.0 + purpose.ttl_ms()),
        };
        let event = Event::PairingCreated {
            pairing_id: pairing.id,
            purpose,
            expires_at: pairing.expires_at,
            created_by: cause.actor.clone(),
        };
        let mut batch = Batch::new(now);
        self.push_event(&mut batch, &event, None, cause, now);
        if let Some(a) = action {
            let reply = (a.respond)(Produced::Pairing(&pairing));
            batch.action = Some(a.record(self, cause, None, Some(pairing.id.0), reply));
        }
        batch.new_pairings.push(NewPairing {
            pairing: pairing.clone(),
            code_sha256: digest(code.expose()),
        });
        self.commit(&batch)?;
        Ok(PairingIssued { pairing, code })
    }

    pub fn claim_pairing(
        &self,
        code: &str,
        purposes: &[PairingPurpose],
    ) -> Result<saka_store::identity::ClaimedPairing> {
        let _guard = self.locked()?;
        let now = self.now();
        let hash = digest(&normalize_code(code).unwrap_or_else(|| code.to_string()));
        self.store
            .transact(|tx| rows::claim_pairing(tx, &hash, purposes, now))?
            .ok_or(EngineError::Rejected(Rejection::PairingCodeInvalid))
    }

    pub fn redeem_device_pairing(
        &self,
        code: &str,
        name: &str,
        correlation_id: Uuid,
    ) -> Result<DeviceIssued> {
        if !name_valid(name) {
            return Err(EngineError::Invalid {
                field: "name",
                code: "invalid".into(),
            });
        }
        let claimed = self.claim_pairing(code, &[PairingPurpose::Token])?;
        let _guard = self.locked()?;
        let now = self.now();
        let token = self.device_token()?;
        let device = Device {
            id: DeviceId(self.new_id()),
            name: name.to_string(),
            kind: DeviceKind::Client,
            created_at: now,
            last_used_at: None,
            revoked_at: None,
        };
        let cause = Cause {
            actor: Actor::Device(device.id).to_string(),
            causation_id: Some(claimed.id.0),
            correlation_id,
        };
        let event = Event::DevicePaired {
            device_id: device.id,
            name: device.name.clone(),
            kind: device.kind,
            pairing_id: Some(claimed.id),
        };
        let mut batch = Batch::new(now);
        self.push_event(&mut batch, &event, None, &cause, now);
        batch.new_devices.push(NewDevice {
            device: device.clone(),
            token_sha256: digest(token.expose()),
        });
        self.commit(&batch)?;
        Ok(DeviceIssued { device, token })
    }

    pub fn devices(&self) -> Result<Vec<Device>> {
        Ok(self.store.read(rows::active_devices)?)
    }

    pub fn revoke_device(
        &self,
        id: DeviceId,
        cause: &Cause,
        action: Option<&Action<'_>>,
    ) -> Result<Device> {
        let _guard = self.locked()?;
        let now = self.now();
        let mut device = self
            .store
            .read(|c| rows::device(c, id))?
            .ok_or(EngineError::Rejected(Rejection::NotFound))?;
        if device.kind == DeviceKind::LocalCli {
            return Err(Rejection::DeviceNotRevocable.into());
        }
        let mut batch = Batch::new(now);
        if device.revoked_at.is_none() {
            self.push_event(
                &mut batch,
                &Event::DeviceRevoked { device_id: id },
                None,
                cause,
                now,
            );
            batch.revoked_devices.push(id);
            device.revoked_at = Some(now);
        }
        if let Some(a) = action {
            let reply = (a.respond)(Produced::Device(&device));
            batch.action = Some(a.record(self, cause, None, Some(id.0), reply));
        }
        self.commit(&batch)?;
        Ok(device)
    }

    pub fn credentials(&self) -> Result<Vec<Credential>> {
        Ok(self.store.read(rows::active_credential_list)?)
    }

    pub fn setup_required(&self) -> Result<bool> {
        Ok(self.store.read(rows::active_credentials)? == 0)
    }
}
