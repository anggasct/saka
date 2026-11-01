//! The `saka` binary: the server (`serve`, `migrate`), the session runner, and the thin
//! operator commands that call the server's API.

mod cli;
mod config;
mod logging;
mod serve;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};

use crate::cli::Client;
use crate::config::Config;

#[derive(Parser)]
#[command(
    name = "saka",
    version,
    about = "A self-hosted delivery loop for coding agents."
)]
struct Cli {
    /// The server to talk to (default: `server.listen` from the config).
    #[arg(long, global = true, env = "SAKA_SERVER")]
    server: Option<String>,
    /// Print the API response body as JSON.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Runs the server: API, workers, and reconciler.
    Serve,
    /// Applies pending database migrations (after a backup) and exits.
    Migrate,
    /// Runs one agent session (started by the server, not by hand).
    #[command(hide = true)]
    Runner {
        #[arg(long)]
        session: String,
        #[arg(long)]
        launch: PathBuf,
    },
    /// Projects.
    #[command(subcommand)]
    Project(ProjectCmd),
    /// Specs.
    #[command(subcommand)]
    Spec(SpecCmd),
    /// Tasks.
    #[command(subcommand)]
    Task(TaskCmd),
    /// Permission requests from agent sessions.
    #[command(subcommand)]
    Permission(PermissionCmd),
}

#[derive(Subcommand)]
enum ProjectCmd {
    /// Registers the project folder of a repo (its `saka.toml`).
    Add {
        /// HTTPS URL of the repo.
        workspace_url: String,
        /// The project folder, `.` for the repo root.
        #[arg(long, default_value = "docs")]
        path: String,
        /// Default: the repo's default branch.
        #[arg(long)]
        base_branch: Option<String>,
    },
    List,
}

#[derive(Subcommand)]
enum SpecCmd {
    /// Reads the spec at the tip of the base branch and approves it.
    Approve {
        /// Project id or name.
        project: String,
        /// Path of the spec file from the repo root.
        path: String,
        #[arg(long)]
        reason: Option<String>,
    },
}

#[derive(Subcommand)]
enum TaskCmd {
    List {
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        spec: Option<String>,
        /// One or more states, comma-separated.
        #[arg(long)]
        state: Option<String>,
    },
    /// Merges the task's change at the head whose CI passed.
    Merge {
        id: String,
        #[arg(long)]
        reason: Option<String>,
    },
    /// Tries the stopped stage again.
    Retry {
        id: String,
        #[arg(long)]
        reason: String,
    },
    Cancel {
        id: String,
        #[arg(long)]
        reason: String,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum Decision {
    Allow,
    Deny,
}

#[derive(Subcommand)]
enum PermissionCmd {
    List {
        #[arg(long, default_value = "pending")]
        state: String,
    },
    Answer {
        id: String,
        decision: Decision,
        /// Sent to the agent with a deny.
        #[arg(long)]
        message: Option<String>,
    },
}

fn load_config() -> Result<Config, ExitCode> {
    Config::load().map_err(|e| {
        tracing::error!(code = e.code, detail = %e.detail, "config refused");
        ExitCode::from(serve::EXIT_REFUSED)
    })
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

fn main() -> ExitCode {
    let cli = Cli::parse();
    logging::init();
    match run(cli) {
        Ok(code) | Err(code) => code,
    }
}

fn run(cli: Cli) -> Result<ExitCode, ExitCode> {
    let server = |cli_server: Option<String>| -> Result<String, ExitCode> {
        match cli_server {
            Some(s) => Ok(s),
            None => Ok(format!("http://{}", load_config()?.server.listen)),
        }
    };
    Ok(match cli.command {
        Command::Serve => serve::serve(&load_config()?),
        Command::Migrate => serve::migrate(&load_config()?),
        Command::Runner { session, launch } => {
            let code = current_thread()?.block_on(saka_agents::runner::run(&session, &launch));
            ExitCode::from(u8::try_from(code).unwrap_or(1))
        }
        Command::Project(cmd) => {
            let client = Client::new(&server(cli.server)?, cli.json);
            current_thread()?.block_on(async {
                match cmd {
                    ProjectCmd::Add {
                        workspace_url,
                        path,
                        base_branch,
                    } => {
                        client
                            .project_add(&workspace_url, &path, base_branch.as_deref())
                            .await
                    }
                    ProjectCmd::List => client.project_list().await,
                }
            })
        }
        Command::Spec(SpecCmd::Approve {
            project,
            path,
            reason,
        }) => {
            let client = Client::new(&server(cli.server)?, cli.json);
            current_thread()?.block_on(client.spec_approve(&project, &path, reason.as_deref()))
        }
        Command::Task(cmd) => {
            let client = Client::new(&server(cli.server)?, cli.json);
            current_thread()?.block_on(async {
                match cmd {
                    TaskCmd::List {
                        project,
                        spec,
                        state,
                    } => {
                        client
                            .task_list(project.as_deref(), spec.as_deref(), state.as_deref())
                            .await
                    }
                    TaskCmd::Merge { id, reason } => {
                        client.task_merge(&id, reason.as_deref()).await
                    }
                    TaskCmd::Retry { id, reason } => {
                        client.task_action(&id, "retry", &reason).await
                    }
                    TaskCmd::Cancel { id, reason } => {
                        client.task_action(&id, "cancel", &reason).await
                    }
                }
            })
        }
        Command::Permission(cmd) => {
            let client = Client::new(&server(cli.server)?, cli.json);
            current_thread()?.block_on(async {
                match cmd {
                    PermissionCmd::List { state } => client.permission_list(&state).await,
                    PermissionCmd::Answer {
                        id,
                        decision,
                        message,
                    } => {
                        let d = match decision {
                            Decision::Allow => "allow",
                            Decision::Deny => "deny",
                        };
                        client.permission_answer(&id, d, message.as_deref()).await
                    }
                }
            })
        }
    })
}
