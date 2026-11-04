use std::ffi::OsString;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use saka_agents::account;
use saka_agents::launch::ClaudeSettings;
use saka_core::diagnostics::AgentLogin;
use serde_json::{Value, json};

use crate::cli;
use crate::config::Config;
use crate::doctor::account_of;
use crate::serve::{self, EXIT_REFUSED};

pub const LOGIN_ENV: &[&str] = &[
    "HOME",
    "TERM",
    "LANG",
    "LC_ALL",
    "DISPLAY",
    "WAYLAND_DISPLAY",
    "XDG_RUNTIME_DIR",
    "DBUS_SESSION_BUS_ADDRESS",
    "BROWSER",
];

fn refuse(code: &str, params: &Value, exit: u8) -> ExitCode {
    cli::refuse(code, params);
    ExitCode::from(exit)
}

fn not_found(settings: &ClaudeSettings) -> ExitCode {
    refuse(
        "agent_cli_not_found",
        &json!({ "command": settings.command }),
        1,
    )
}

fn personal_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("HOME")
        .map(|h| vec![PathBuf::from(h).join(".claude")])
        .unwrap_or_default();
    dirs.extend(std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from));
    dirs
}

fn same_dir(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

fn prepare_dir(dir: &Path) -> Result<(), ExitCode> {
    let path = || json!({ "path": dir.display().to_string() });
    if personal_dirs().iter().any(|p| same_dir(p, dir)) {
        return Err(refuse("account_dir_personal", &path(), EXIT_REFUSED));
    }
    match std::fs::symlink_metadata(dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .map_err(|e| {
                refuse(
                    "account_dir_unwritable",
                    &json!({ "path": dir.display().to_string(), "reason": e.to_string() }),
                    1,
                )
            }),
        Err(e) => Err(refuse(
            "account_dir_unwritable",
            &json!({ "path": dir.display().to_string(), "reason": e.to_string() }),
            1,
        )),
        Ok(meta) if !meta.is_dir() => Err(refuse("account_dir_not_directory", &path(), 1)),
        Ok(meta) if meta.uid() != rustix::process::getuid().as_raw() => {
            Err(refuse("account_dir_not_owned", &path(), 1))
        }
        Ok(meta) if meta.mode() & 0o077 != 0 => {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).map_err(|e| {
                refuse(
                    "account_dir_unwritable",
                    &json!({ "path": dir.display().to_string(), "reason": e.to_string() }),
                    1,
                )
            })
        }
        Ok(_) => Ok(()),
    }
}

pub fn login_command(
    program: &Path,
    settings: &ClaudeSettings,
    console: bool,
    operator: impl Fn(&str) -> Option<OsString>,
) -> Command {
    let mut command = Command::new(program);
    command.args(["auth", "login"]);
    if console {
        command.arg("--console");
    }
    command
        .env_clear()
        .env("PATH", account::search_path(settings));
    for key in LOGIN_ENV {
        if let Some(value) = operator(key) {
            command.env(key, value);
        }
    }
    command.env("CLAUDE_CONFIG_DIR", &settings.account_dir);
    command
}

fn current_thread() -> Result<tokio::runtime::Runtime, ExitCode> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| {
            tracing::error!(error = %e, "cannot start the runtime");
            ExitCode::FAILURE
        })
}

pub fn login(cfg: &Config, console: bool, force: bool) -> ExitCode {
    let settings = serve::claude_settings(cfg);
    let label = account_of(cfg);
    let Some(program) = settings.program() else {
        return not_found(&settings);
    };
    if let Err(code) = prepare_dir(Path::new(&settings.account_dir)) {
        return code;
    }
    let rt = match current_thread() {
        Ok(rt) => rt,
        Err(code) => return code,
    };
    let before = rt.block_on(account::login(&settings, account::STATUS_TIMEOUT));
    if let AgentLogin::LoggedIn { auth_method } = &before
        && !force
    {
        cli::out(&format!(
            "account {label} already logged in ({auth_method})"
        ));
        return ExitCode::SUCCESS;
    }
    let status = login_command(&program, &settings, console, |k| std::env::var_os(k)).status();
    let exit_code = status
        .as_ref()
        .ok()
        .and_then(std::process::ExitStatus::code);
    match rt.block_on(account::login(&settings, account::STATUS_TIMEOUT)) {
        AgentLogin::LoggedIn { auth_method } => {
            cli::out(&format!("account {label} logged in ({auth_method})"));
            ExitCode::SUCCESS
        }
        _ => refuse("agent_login_failed", &json!({ "exit_code": exit_code }), 1),
    }
}

pub fn status(cfg: &Config, json: bool) -> ExitCode {
    let settings = serve::claude_settings(cfg);
    let label = account_of(cfg);
    let rt = match current_thread() {
        Ok(rt) => rt,
        Err(code) => return code,
    };
    let (found, login) = rt.block_on(async {
        let found = account::cli(&settings).await.found;
        let login = account::login(&settings, account::STATUS_TIMEOUT).await;
        (found, login)
    });
    let (logged_in, auth_method) = match &login {
        AgentLogin::LoggedIn { auth_method } => (true, Some(auth_method.clone())),
        _ => (false, None),
    };
    if json {
        cli::out(
            &json!({
                "account": label,
                "config_dir": settings.account_dir,
                "cli": found.as_ref().map(|f| json!({ "path": f.path, "version": f.version })),
                "logged_in": logged_in,
                "auth_method": auth_method,
            })
            .to_string(),
        );
    }
    match login {
        AgentLogin::CliNotFound { .. } => {
            if json {
                ExitCode::FAILURE
            } else {
                not_found(&settings)
            }
        }
        AgentLogin::LoggedIn { auth_method } => {
            if !json {
                cli::out(&format!("{label} logged in ({auth_method})"));
            }
            ExitCode::SUCCESS
        }
        AgentLogin::NotLoggedIn => {
            if !json {
                cli::out(&format!("{label} not logged in"));
            }
            ExitCode::FAILURE
        }
        AgentLogin::Unavailable { exit_code } => {
            if json {
                ExitCode::FAILURE
            } else {
                refuse(
                    "agent_status_unavailable",
                    &json!({ "account": label, "exit_code": exit_code }),
                    1,
                )
            }
        }
    }
}
