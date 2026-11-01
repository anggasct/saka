//! Composition root for the `saka` binary.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "saka",
    version,
    about = "A self-hosted delivery loop for coding agents."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Runs one agent session (started by the server, not by hand).
    #[command(hide = true)]
    Runner {
        #[arg(long)]
        session: String,
        #[arg(long)]
        launch: PathBuf,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    tracing_subscriber::fmt()
        .json()
        .with_writer(std::io::stderr)
        .with_current_span(false)
        .init();
    match cli.command {
        Command::Runner { session, launch } => {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    tracing::error!(error = %e, "cannot start the runtime");
                    return ExitCode::FAILURE;
                }
            };
            let code = runtime.block_on(saka_agents::runner::run(&session, &launch));
            ExitCode::from(u8::try_from(code).unwrap_or(1))
        }
    }
}
