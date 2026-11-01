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
    #[arg(
        help = "The server to talk to (default: `server.listen` from the config)",
        long,
        global = true,
        env = "SAKA_SERVER"
    )]
    server: Option<String>,
    #[arg(help = "Print the API response body as JSON", long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    #[command(about = "Runs the server: API, workers, and reconciler")]
    Serve,
    #[command(about = "Applies pending database migrations (after a backup) and exits")]
    Migrate,
    #[command(
        about = "Runs one agent session (started by the server, not by hand)",
        hide = true
    )]
    Runner {
        #[arg(long)]
        session: String,
        #[arg(long)]
        launch: PathBuf,
    },
    #[command(about = "Projects")]
    #[command(subcommand)]
    Project(ProjectCmd),
    #[command(about = "Specs")]
    #[command(subcommand)]
    Spec(SpecCmd),
    #[command(about = "Tasks")]
    #[command(subcommand)]
    Task(TaskCmd),
    #[command(about = "Permission requests from agent sessions")]
    #[command(subcommand)]
    Permission(PermissionCmd),
}

#[derive(Subcommand)]
enum ProjectCmd {
    #[command(about = "Registers the project folder of a repo (its `saka.toml`)")]
    Add {
        #[arg(help = "HTTPS URL of the repo")]
        workspace_url: String,
        #[arg(
            help = "The project folder, `.` for the repo root",
            long,
            default_value = "docs"
        )]
        path: String,
        #[arg(help = "Default: the repo's default branch", long)]
        base_branch: Option<String>,
    },
    List,
}

#[derive(Subcommand)]
enum SpecCmd {
    #[command(about = "Reads the spec at the tip of the base branch and approves it")]
    Approve {
        #[arg(help = "Project id or name")]
        project: String,
        #[arg(help = "Path of the spec file from the repo root")]
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
        #[arg(help = "One or more states, comma-separated", long)]
        state: Option<String>,
    },
    #[command(about = "Merges the task's change at the head whose CI passed")]
    Merge {
        id: String,
        #[arg(long)]
        reason: Option<String>,
    },
    #[command(about = "Tries the stopped stage again")]
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
        #[arg(help = "Sent to the agent with a deny", long)]
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
