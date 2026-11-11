mod files;

use std::path::{Path, PathBuf};

use reqwest::Url;
use serde_json::{Value, json};

use crate::cli;
use crate::config::{self, Config};
use crate::serve::EXIT_REFUSED;

#[cfg(test)]
pub(crate) use files::appended;
pub(crate) use files::{
    ConfigBlock, Credentials, PRIVATE_KEY_FILE, WEBHOOK_SECRET_FILE, WriteError, block_text,
    write_new_private_file,
};

pub(crate) const EXIT_FAILED: u8 = 1;
pub(crate) const DEFAULT_HOOKS_LISTEN: &str = "127.0.0.1:7421";
const MAX_ID_LEN: usize = 32;

#[derive(Debug)]
pub(crate) struct Failure {
    pub(crate) exit: u8,
    pub(crate) code: String,
    pub(crate) params: Value,
    pub(crate) block: Option<String>,
}

impl Failure {
    pub(crate) fn refused(code: &str, params: Value) -> Failure {
        Failure {
            exit: EXIT_REFUSED,
            code: code.into(),
            params,
            block: None,
        }
    }

    pub(crate) fn failed(code: &str, params: Value) -> Failure {
        Failure {
            exit: EXIT_FAILED,
            code: code.into(),
            params,
            block: None,
        }
    }

    pub(crate) fn flag(flag: &str, reason: &str) -> Failure {
        Failure::refused("flag_invalid", json!({ "flag": flag, "reason": reason }))
    }

    pub(crate) fn config(e: &config::ConfigError, path: &Path) -> Failure {
        Failure::refused(
            e.code,
            json!({ "path": path.display().to_string(), "detail": e.detail }),
        )
    }
}

pub(crate) fn print_failure(f: &Failure, json_out: bool) {
    cli::refuse(&f.code, &f.params);
    if let Some(block) = &f.block {
        cli::err("add this to the config by hand:");
        cli::err(block.trim_end());
    }
    if json_out {
        cli::out(&json!({ "code": f.code, "params": f.params, "exit_code": f.exit }).to_string());
    }
}

pub(crate) fn id_ok(id: &str) -> bool {
    (1..=MAX_ID_LEN).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

pub(crate) fn provider_url(flag: &str, raw: &str) -> Result<String, Failure> {
    if !config::url_ok(raw) {
        return Err(Failure::flag(flag, "not_https_or_loopback"));
    }
    Ok(raw.trim_end_matches('/').to_string())
}

pub(crate) fn path_text(path: &Path) -> Result<&str, Failure> {
    path.to_str().ok_or_else(|| {
        Failure::refused("config_path_unknown", json!({ "reason": "path_not_utf8" }))
    })
}

fn same_host(a: &str, b: &str) -> bool {
    let norm = |raw: &str| {
        Url::parse(raw).map_or_else(
            |_| raw.trim_end_matches('/').to_ascii_lowercase(),
            |u| u.as_str().trim_end_matches('/').to_string(),
        )
    };
    norm(a) == norm(b)
}

pub(crate) struct OperatorConfig {
    pub(crate) path: PathBuf,
    original: Option<Vec<u8>>,
    text: String,
    cfg: Config,
}

impl OperatorConfig {
    pub(crate) fn load(explicit: Option<&Path>) -> Result<OperatorConfig, Failure> {
        let path = explicit
            .map(Path::to_path_buf)
            .or_else(config::path)
            .ok_or_else(|| Failure::refused("config_path_unknown", json!({})))?;
        let path = std::path::absolute(&path)
            .map_err(|_| Failure::refused("config_path_unknown", json!({})))?;
        let original = files::read_optional(&path).map_err(|e| {
            Failure::refused(
                "config_unreadable",
                json!({ "path": path.display().to_string(), "detail": e.kind().to_string() }),
            )
        })?;
        let text = match &original {
            Some(bytes) => String::from_utf8(bytes.clone()).map_err(|_| {
                Failure::refused(
                    "config_unreadable",
                    json!({ "path": path.display().to_string(), "detail": "not_utf8" }),
                )
            })?,
            None => String::new(),
        };
        let cfg = Config::parse(&text).map_err(|e| Failure::config(&e, &path))?;
        Ok(OperatorConfig {
            path,
            original,
            text,
            cfg,
        })
    }

    pub(crate) fn dir(&self) -> Result<&Path, Failure> {
        self.path
            .parent()
            .ok_or_else(|| Failure::refused("config_path_unknown", json!({})))
    }

    pub(crate) fn created(&self) -> bool {
        self.original.is_none()
    }

    pub(crate) fn has_hooks(&self) -> bool {
        self.cfg.hooks.is_some()
    }

    pub(crate) fn check_free(&self, id: &str, web_url: &str) -> Result<(), Failure> {
        if self.cfg.scm.iter().any(|s| s.id == id) {
            return Err(Failure::refused(
                "scm_integration_exists",
                json!({ "id": id }),
            ));
        }
        match self.cfg.scm.iter().find(|s| same_host(&s.web_url, web_url)) {
            Some(other) => Err(Failure::refused(
                "scm_host_configured",
                json!({ "id": other.id, "web_url": other.web_url }),
            )),
            None => Ok(()),
        }
    }

    pub(crate) fn check_block(&self, block_text: &str) -> Result<(), Failure> {
        Config::parse(&files::appended(&self.text, block_text))
            .map(drop)
            .map_err(|e| Failure::config(&e, &self.path))
    }

    pub(crate) fn write(
        &self,
        block_text: &str,
        known: Option<(&str, &Value)>,
    ) -> Result<(), Failure> {
        let shown = self.path.display().to_string();
        let failure = |code: &str, reason: Option<String>| {
            let mut params = json!({ "path": shown });
            if let Some(map) = params.as_object_mut() {
                if let Some(reason) = reason {
                    map.insert("reason".into(), json!(reason));
                }
                if let Some((key, value)) = known {
                    map.insert(key.to_string(), value.clone());
                }
            }
            let mut f = Failure::failed(code, params);
            f.block = Some(block_text.to_string());
            f
        };
        let now = files::read_optional(&self.path)
            .map_err(|e| failure("config_write_failed", Some(e.kind().to_string())))?;
        if now != self.original {
            return Err(failure("config_changed_concurrently", None));
        }
        let text = files::appended(&self.text, block_text);
        if let Err(e) = Config::parse(&text) {
            return Err(failure("config_write_failed", Some(e.code.to_string())));
        }
        files::replace_config(&self.path, &text)
            .map_err(|e| failure("config_write_failed", Some(e.kind().to_string())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts_compare_without_a_trailing_slash_or_case() {
        assert!(same_host("https://GitHub.com/", "https://github.com"));
        assert!(!same_host("https://ghe.example.com", "https://github.com"));
    }
}
