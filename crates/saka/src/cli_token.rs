use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use saka_core::identity::{DEVICE_TOKEN_PREFIX, token_shape_valid};
use saka_core::secret::SecretText;
use serde_json::json;

use crate::cli::refuse;
use crate::config::Config;
use crate::serve::{CLI_TOKEN_FILE, EXIT_REFUSED};

pub const TOKEN_ENV: &str = "SAKA_TOKEN";

fn refused(code: &str, path: &Path) -> ExitCode {
    refuse(code, &json!({ "path": path.display().to_string() }));
    ExitCode::from(EXIT_REFUSED)
}

pub fn read_token_file(path: &Path) -> Result<SecretText, ExitCode> {
    let meta = std::fs::metadata(path).map_err(|_| refused("token_file_unreadable", path))?;
    let mine = meta.uid() == rustix::process::getuid().as_raw();
    if !meta.is_file() || !mine || meta.mode() & 0o077 != 0 {
        return Err(refused("token_file_mode", path));
    }
    let text = std::fs::read_to_string(path).map_err(|_| refused("token_file_unreadable", path))?;
    let token = text.trim();
    if !token_shape_valid(DEVICE_TOKEN_PREFIX, token) {
        return Err(refused("token_file_invalid", path));
    }
    Ok(SecretText::new(token.to_string()))
}

fn loopback_server(server: &str) -> bool {
    reqwest::Url::parse(server)
        .ok()
        .and_then(|u| u.host_str().map(str::to_ascii_lowercase))
        .is_some_and(|host| {
            let host = host.trim_matches(['[', ']']);
            host == "localhost"
                || host
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        })
}

fn default_token_path() -> Option<PathBuf> {
    Config::load()
        .ok()
        .map(|cfg| cfg.data_dir().join(CLI_TOKEN_FILE))
}

pub fn resolve(server: &str, token_file: Option<&Path>) -> Result<Option<SecretText>, ExitCode> {
    if let Some(path) = token_file {
        return read_token_file(path).map(Some);
    }
    if let Ok(value) = std::env::var(TOKEN_ENV) {
        let token = value.trim();
        if !token_shape_valid(DEVICE_TOKEN_PREFIX, token) {
            refuse("token_env_invalid", &json!({ "variable": TOKEN_ENV }));
            return Err(ExitCode::from(EXIT_REFUSED));
        }
        return Ok(Some(SecretText::new(token.to_string())));
    }
    if !loopback_server(server) {
        return Ok(None);
    }
    match default_token_path() {
        Some(path) if path.exists() => read_token_file(&path).map(Some),
        _ => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    const TOKEN: &str = "sdt_0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn only_loopback_servers_get_the_local_token() {
        assert!(loopback_server("http://127.0.0.1:7420"));
        assert!(loopback_server("http://localhost:7420"));
        assert!(loopback_server("http://[::1]:7420"));
        assert!(!loopback_server("https://saka.example.com"));
        assert!(!loopback_server("http://10.0.0.5:7420"));
        assert!(!loopback_server("not a url"));
    }

    #[test]
    fn a_token_file_must_be_private_and_hold_a_token() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cli-token");
        std::fs::write(&path, format!("{TOKEN}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read_token_file(&path).is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(read_token_file(&path).unwrap().expose(), TOKEN);
        std::fs::write(&path, "sdt_short").unwrap();
        assert!(read_token_file(&path).is_err());
        assert!(read_token_file(&dir.path().join("missing")).is_err());
    }
}
