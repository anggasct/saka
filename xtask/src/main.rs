//! Repository tasks, run as `cargo xtask <command>`.

mod check;
mod deps;
mod metadata;

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("check") => check::run(),
        Some("deps") => deps::run(),
        _ => {
            eprintln!("usage: cargo xtask <check|deps>");
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
