mod check;
mod claude_memory;
mod deps;
mod e2e;
mod hygiene;
mod metadata;
mod supply_chain;
mod supply_chain_web;
mod systemd;
mod tools;
mod wasm;

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("check") => check::run(),
        Some("deps") => deps::run(),
        Some("e2e") => e2e::run(),
        Some("hygiene") => metadata::workspace_root().and_then(|root| hygiene::run(&root)),
        Some("supply-chain") => match &args[1..] {
            [] => supply_chain::run(false),
            [flag] if flag == "--offline" => supply_chain::run(true),
            _ => return usage(),
        },
        Some("test-systemd") => systemd::run(),
        Some("test-claude-memory") => claude_memory::run(),
        Some("wasm") => wasm::run(),
        _ => return usage(),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn usage() -> ExitCode {
    eprintln!(
        "usage: cargo xtask <check|deps|e2e|hygiene|supply-chain [--offline]|test-systemd|test-claude-memory|wasm>"
    );
    ExitCode::from(2)
}
