use saka_core::event::Event;
use saka_core::ids::ProjectId;
use saka_core::redact::Redactor;
use saka_core::rejection::Rejection;
use saka_core::secret::{
    PER_PROJECT_MAX, SecretScope, SecretText, SecretVar, VALUE_MAX_BYTES, VALUE_MIN_BYTES,
};
use saka_core::time::Timestamp;
use saka_core::vocab::BlockerCode;
use saka_ports::{PortError, Sealed};
use saka_store::{Batch, SecretRow, read};
use serde_json::json;

use crate::action::{Action, Produced};
use crate::recipe::{self, Recipe};
use crate::worker::Refusal;
use crate::{Cause, Engine, EngineError, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretInfo {
    pub name: String,
    pub updated_at: Timestamp,
}

fn check_name(name: &str) -> Result<()> {
    if recipe::name_ok(name) && !recipe::name_reserved(name) {
        Ok(())
    } else {
        Err(EngineError::Rejected(Rejection::SecretNameInvalid))
    }
}

fn check_value(value: &SecretText) -> Result<()> {
    let text = value.expose();
    if text.len() < VALUE_MIN_BYTES {
        return Err(EngineError::Rejected(Rejection::SecretTooShort));
    }
    let code = if text.len() > VALUE_MAX_BYTES {
        "too_long"
    } else if text.contains('\0') {
        "nul_byte"
    } else {
        return Ok(());
    };
    Err(EngineError::Rejected(Rejection::Invalid {
        field: "value",
        code,
    }))
}

impl Engine {
    fn require_project(&self, project: ProjectId) -> Result<()> {
        self.store
            .read(|c| read::load_project(c, project))?
            .map(|_| ())
            .ok_or(EngineError::Rejected(Rejection::NotFound))
    }

    pub fn secret_fingerprint(&self, project: ProjectId, name: &str, value: &SecretText) -> String {
        let scope = SecretScope::Project {
            project_id: project,
            name,
        };
        self.ports.secrets.fingerprint(&scope, value)
    }

    pub fn secrets(&self, project: ProjectId) -> Result<Vec<SecretInfo>> {
        self.require_project(project)?;
        Ok(self
            .store
            .read(|c| read::project_secrets(c, project))?
            .into_iter()
            .map(|r| SecretInfo {
                name: r.name,
                updated_at: r.updated_at,
            })
            .collect())
    }

    pub fn set_secret(
        &self,
        project: ProjectId,
        name: &str,
        value: &SecretText,
        cause: &Cause,
        action: Option<&Action<'_>>,
    ) -> Result<()> {
        self.require_project(project)?;
        check_name(name)?;
        check_value(value)?;
        let _guard = self.locked()?;
        let now = self.now();
        let existing = self.store.read(|c| read::project_secrets(c, project))?;
        let previous = existing.iter().find(|r| r.name == name);
        if previous.is_none() && existing.len() >= PER_PROJECT_MAX {
            return Err(EngineError::Rejected(Rejection::Invalid {
                field: "name",
                code: "too_many_secrets",
            }));
        }
        let scope = SecretScope::Project {
            project_id: project,
            name,
        };
        let sealed = self.ports.secrets.seal(&scope, value)?;
        let mut batch = Batch::new(now);
        let event = Event::SecretSet {
            project_id: project,
            name: name.to_string(),
        };
        self.push_event(&mut batch, &event, Some(project), cause, now);
        batch.secrets.push(SecretRow {
            project_id: project,
            name: name.to_string(),
            nonce: sealed.nonce,
            ciphertext: sealed.ciphertext,
            created_at: previous.map_or(now, |p| p.created_at),
            updated_at: now,
        });
        if let Some(action) = action {
            let reply = (action.respond)(Produced::Nothing);
            batch.action = Some(action.record(self, cause, Some(project), Some(project.0), reply));
        }
        self.commit(&batch)?;
        tracing::info!(project_id = %project.0, name, "secret set");
        Ok(())
    }

    pub fn remove_secret(
        &self,
        project: ProjectId,
        name: &str,
        cause: &Cause,
        action: Option<&Action<'_>>,
    ) -> Result<()> {
        self.require_project(project)?;
        check_name(name)?;
        let _guard = self.locked()?;
        let now = self.now();
        let exists = self
            .store
            .read(|c| read::project_secrets(c, project))?
            .iter()
            .any(|r| r.name == name);
        let mut batch = Batch::new(now);
        if exists {
            let event = Event::SecretRemoved {
                project_id: project,
                name: name.to_string(),
            };
            self.push_event(&mut batch, &event, Some(project), cause, now);
            batch.removed_secrets.push((project, name.to_string()));
        }
        if let Some(action) = action {
            let reply = (action.respond)(Produced::Nothing);
            batch.action = Some(action.record(self, cause, Some(project), Some(project.0), reply));
        }
        self.commit(&batch)?;
        if exists {
            tracing::info!(project_id = %project.0, name, "secret removed");
        }
        Ok(())
    }

    pub(crate) fn project_redactor(
        &self,
        project: ProjectId,
    ) -> std::result::Result<Redactor, PortError> {
        let rows = self
            .store
            .read(|c| read::project_secrets(c, project))
            .map_err(|e| {
                PortError::retryable("store_unavailable", json!({ "detail": e.to_string() }))
            })?;
        let mut named = Vec::new();
        for row in rows {
            let sealed = Sealed {
                nonce: row.nonce,
                ciphertext: row.ciphertext,
            };
            let scope = SecretScope::Project {
                project_id: project,
                name: &row.name,
            };
            match self.ports.secrets.open(&scope, &sealed) {
                Ok(value) => named.push((row.name, value.expose().to_string())),
                Err(e) if e.retryable => return Err(e),
                Err(_) => {}
            }
        }
        Ok(Redactor::named(&[], &named))
    }

    pub(crate) fn recipe_secrets(
        &self,
        project: ProjectId,
        recipe: &Recipe,
    ) -> std::result::Result<Vec<SecretVar>, Refusal> {
        if recipe.secrets.is_empty() {
            return Ok(Vec::new());
        }
        let rows = self
            .store
            .read(|c| read::project_secrets(c, project))
            .map_err(|e| {
                Refusal::Fail(PortError::retryable(
                    "store_unavailable",
                    json!({ "detail": e.to_string() }),
                ))
            })?;
        let missing: Vec<&String> = recipe
            .secrets
            .keys()
            .filter(|name| !rows.iter().any(|r| &r.name == *name))
            .collect();
        if !missing.is_empty() {
            return Err(Refusal::Block(
                BlockerCode::SecretMissing,
                json!({ "names": missing }),
            ));
        }
        let mut out = Vec::new();
        for (name, purposes) in &recipe.secrets {
            let Some(row) = rows.iter().find(|r| &r.name == name) else {
                continue;
            };
            let sealed = Sealed {
                nonce: row.nonce.clone(),
                ciphertext: row.ciphertext.clone(),
            };
            let scope = SecretScope::Project {
                project_id: project,
                name,
            };
            match self.ports.secrets.open(&scope, &sealed) {
                Ok(value) => out.push(SecretVar {
                    name: name.clone(),
                    value,
                    purposes: purposes.clone(),
                }),
                Err(e) if !e.retryable => {
                    return Err(Refusal::Block(
                        BlockerCode::SecretUnreadable,
                        json!({ "name": name }),
                    ));
                }
                Err(e) => return Err(Refusal::Fail(e)),
            }
        }
        Ok(out)
    }
}
