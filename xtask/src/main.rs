mod check;
mod deps;
mod hygiene;
mod metadata;
mod systemd;

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("check") => check::run(),
        Some("deps") => deps::run(),
        Some("hygiene") => metadata::workspace_root().and_then(|root| hygiene::run(&root)),
        Some("test-systemd") => systemd::run(),
        _ => {
            eprintln!("usage: cargo xtask <check|deps|hygiene|test-systemd>");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
        }
    }
}
