use saka_core::event::Event;
use saka_core::identity::{
    Actor, Credential, DEVICE_TOKEN_PREFIX, Device, Pairing, SECRET_BYTES, SESSION_TOKEN_PREFIX,
    TOUCH_INTERVAL_MS, name_valid, normalize_code, pairing_code, secret_token, token_shape_valid,
};
use saka_core::ids::{CredentialId, DeviceId, LoginSessionId, PairingId};
use saka_core::rejection::Rejection;
use saka_core::secret::SecretText;
use saka_core::time::Timestamp;
use saka_core::vocab::{DeviceKind, PairingPurpose};
use saka_store::Batch;
use saka_store::identity::{
    self as rows, CredentialLogin, NewCredential, NewDevice, NewPairing, NewSession,
};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::action::{Action, Produced};
use crate::{Cause, Engine, EngineError, Result};

const LOCAL_CLI_NAME: &str = "Local CLI";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Principal {
    pub actor: Actor,
    pub device_kind: Option<DeviceKind>,
    pub session: Option<LoginSessionId>,
}

#[derive(Debug, Clone)]
pub struct SessionIssued {
    pub session_id: LoginSessionId,
    pub token: SecretText,
    pub expires_at: Timestamp,
}

#[derive(Debug, Clone)]
pub struct Registered {
    pub credential: Credential,
    pub session: SessionIssued,
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
            session: None,
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

    pub fn ceremony_id(&self) -> Result<String> {
        Ok(self
            .random::<16>()?
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect())
    }

    pub fn authenticate_session(&self, cookie: &str) -> Result<Option<Principal>> {
        if !token_shape_valid(SESSION_TOKEN_PREFIX, cookie) {
            return Ok(None);
        }
        let hash = digest(cookie);
        let now = self.now();
        Ok(self
            .store
            .read(|c| rows::session_by_token(c, &hash, now))?
            .map(|hit| Principal {
                actor: Actor::Passkey(hit.credential_id),
                device_kind: None,
                session: Some(hit.id),
            }))
    }

    pub fn passkeys(&self) -> Result<Vec<(CredentialId, serde_json::Value)>> {
        self.store
            .read(rows::active_passkeys)?
            .into_iter()
            .map(|(id, json)| {
                serde_json::from_str(&json)
                    .map(|value| (id, value))
                    .map_err(|_| EngineError::Internal("stored passkey is not json".into()))
            })
            .collect()
    }

    pub fn check_pairing_code(&self, code: &str, purposes: &[PairingPurpose]) -> Result<()> {
        let _guard = self.locked()?;
        let now = self.now();
        let hash = digest(&normalize_code(code).unwrap_or_else(|| code.to_string()));
        self.store
            .transact(|tx| rows::peek_pairing(tx, &hash, purposes, now))?
            .map(|_| ())
            .ok_or(EngineError::Rejected(Rejection::PairingCodeInvalid))
    }

    fn open_session(
        &self,
        batch: &mut Batch,
        credential_id: CredentialId,
        ttl_ms: i64,
        correlation_id: Uuid,
    ) -> Result<SessionIssued> {
        let now = self.now();
        let entropy = self.random::<SECRET_BYTES>()?;
        let token = SecretText::new(secret_token(SESSION_TOKEN_PREFIX, &entropy));
        let session_id = LoginSessionId(self.new_id());
        let expires_at = Timestamp(now.0.saturating_add(ttl_ms));
        let cause = Cause {
            actor: Actor::Passkey(credential_id).to_string(),
            causation_id: None,
            correlation_id,
        };
        let event = Event::LoginSessionOpened {
            session_id,
            credential_id,
            expires_at,
        };
        self.push_event(batch, &event, None, &cause, now);
        batch.new_sessions.push(NewSession {
            id: session_id,
            credential_id,
            token_sha256: digest(token.expose()),
            created_at: now,
            expires_at,
        });
        Ok(SessionIssued {
            session_id,
            token,
            expires_at,
        })
    }

    pub fn register_credential(
        &self,
        name: &str,
        passkey: &serde_json::Value,
        code: Option<&str>,
        ttl_ms: i64,
        correlation_id: Uuid,
    ) -> Result<Registered> {
        if !name_valid(name) {
            return Err(EngineError::Invalid {
                field: "name",
                code: "invalid".into(),
            });
        }
        let pairing_id = match code {
            Some(code) => Some(
                self.claim_pairing(code, &[PairingPurpose::Setup, PairingPurpose::Passkey])?
                    .id,
            ),
            None => None,
        };
        let _guard = self.locked()?;
        let now = self.now();
        let credential = Credential {
            id: CredentialId(self.new_id()),
            name: name.to_string(),
            created_at: now,
            last_used_at: None,
            revoked_at: None,
        };
        let cause = Cause {
            actor: Actor::Passkey(credential.id).to_string(),
            causation_id: pairing_id.map(|p| p.0),
            correlation_id,
        };
        let mut batch = Batch::new(now);
        let event = Event::CredentialRegistered {
            credential_id: credential.id,
            name: credential.name.clone(),
            pairing_id,
        };
        self.push_event(&mut batch, &event, None, &cause, now);
        batch.new_credentials.push(NewCredential {
            credential: credential.clone(),
            passkey: passkey.to_string(),
        });
        let session = self.open_session(&mut batch, credential.id, ttl_ms, correlation_id)?;
        self.commit(&batch)?;
        Ok(Registered {
            credential,
            session,
        })
    }

    pub fn login(
        &self,
        credential_id: CredentialId,
        passkey: Option<&serde_json::Value>,
        ttl_ms: i64,
        correlation_id: Uuid,
    ) -> Result<SessionIssued> {
        let _guard = self.locked()?;
        let now = self.now();
        let active = self
            .store
            .read(|c| rows::credential(c, credential_id))?
            .is_some_and(|c| c.revoked_at.is_none());
        if !active {
            return Err(EngineError::Rejected(Rejection::NotFound));
        }
        let mut batch = Batch::new(now);
        batch.credential_logins.push(CredentialLogin {
            id: credential_id,
            at: now,
            passkey: passkey.map(ToString::to_string),
        });
        let session = self.open_session(&mut batch, credential_id, ttl_ms, correlation_id)?;
        self.commit(&batch)?;
        Ok(session)
    }

    pub fn close_session(&self, principal: &Principal, cause: &Cause) -> Result<()> {
        let (Some(session_id), Actor::Passkey(credential_id)) =
            (principal.session, &principal.actor)
        else {
            return Err(EngineError::Rejected(Rejection::NotFound));
        };
        let _guard = self.locked()?;
        let now = self.now();
        let mut batch = Batch::new(now);
        let event = Event::LoginSessionClosed {
            session_id,
            credential_id: *credential_id,
        };
        self.push_event(&mut batch, &event, None, cause, now);
        batch.revoked_sessions.push(session_id);
        self.commit(&batch)?;
        Ok(())
    }

    pub fn revoke_credential(
        &self,
        id: CredentialId,
        cause: &Cause,
        action: Option<&Action<'_>>,
    ) -> Result<Credential> {
        let _guard = self.locked()?;
        let now = self.now();
        let mut credential = self
            .store
            .read(|c| rows::credential(c, id))?
            .ok_or(EngineError::Rejected(Rejection::NotFound))?;
        let mut batch = Batch::new(now);
        if credential.revoked_at.is_none() {
            if self.store.read(rows::active_credentials)? <= 1 {
                return Err(Rejection::LastCredential.into());
            }
            self.push_event(
                &mut batch,
                &Event::CredentialRevoked { credential_id: id },
                None,
                cause,
                now,
            );
            batch.revoked_credentials.push(id);
            credential.revoked_at = Some(now);
        }
        if let Some(a) = action {
            let reply = (a.respond)(Produced::Credential(&credential));
            batch.action = Some(a.record(self, cause, None, Some(id.0), reply));
        }
        self.commit(&batch)?;
        Ok(credential)
    }
}
