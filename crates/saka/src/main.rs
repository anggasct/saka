mod cli;
mod config;
mod init;
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
    #[command(
        about = "Sets up this repo for Saka: docs location, saka.toml, toolchain versions, and a setup recipe; registers the project once saka.toml is pushed",
        after_help = "Exit codes: 0 done, 1 failed, 2 usage or environment, 3 config problems, 5 waiting for a push. Saka never commits or pushes."
    )]
    Init(init::InitArgs),
    #[command(about = "Projects")]
    #[command(subcommand)]
    Project(ProjectCmd),
    #[command(about = "Specs")]
    #[command(subcommand)]
    Spec(SpecCmd),
    #[command(about = "Tasks")]
    #[command(subcommand)]
    Task(TaskCmd),
    #[command(about = "Spec drafts the architect writes from an idea")]
    #[command(subcommand)]
    Draft(DraftCmd),
    #[command(about = "Permission requests from agent sessions")]
    #[command(subcommand)]
    Permission(PermissionCmd),
    #[command(about = "Project secrets for environment recipes (write-only)")]
    #[command(subcommand)]
    Secret(SecretCmd),
    #[command(
        about = "Your account at an SCM integration, so agent changes can be authored as you"
    )]
    #[command(subcommand)]
    Scm(ScmCmd),
}

#[derive(Subcommand)]
enum ScmCmd {
    #[command(
        about = "Links your account with the device flow: prints a code to enter at the provider, then waits"
    )]
    Link {
        #[arg(help = "The `[[scm]].id`; optional when the config has exactly one")]
        integration: Option<String>,
        #[arg(help = "Print the code and exit without waiting for the link", long)]
        no_wait: bool,
    },
    #[command(about = "Shows the linked account; exits 1 when there is none")]
    Status {
        #[arg(help = "The `[[scm]].id`; optional when the config has exactly one")]
        integration: Option<String>,
    },
    #[command(about = "Removes the link; revoke the tokens in the provider's settings")]
    Unlink {
        #[arg(help = "The `[[scm]].id`; optional when the config has exactly one")]
        integration: Option<String>,
    },
}

fn integration_of(given: Option<String>) -> Result<String, ExitCode> {
    if let Some(id) = given {
        return Ok(id);
    }
    let cfg = load_config()?;
    match cfg.scm.as_slice() {
        [only] => Ok(only.id.clone()),
        all => {
            let ids: Vec<&str> = all.iter().map(|s| s.id.as_str()).collect();
            cli::refuse(
                "integration_required",
                &serde_json::json!({ "integrations": ids }),
            );
            Err(ExitCode::from(serve::EXIT_REFUSED))
        }
    }
}

#[derive(Subcommand)]
enum SecretCmd {
    #[command(about = "Stores a secret; the value is read from stdin, never from the command line")]
    Set {
        #[arg(help = "Project id or name")]
        project: String,
        #[arg(help = "Secret name, for example NPM_TOKEN")]
        name: String,
        #[arg(
            help = "Store all of stdin as is, instead of the first line without its newline",
            long
        )]
        multiline: bool,
    },
    #[command(about = "Lists secret names; values cannot be read back")]
    List {
        #[arg(help = "Project id or name")]
        project: String,
    },
    #[command(about = "Removes a secret")]
    Rm {
        #[arg(help = "Project id or name")]
        project: String,
        name: String,
    },
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
    #[command(
        about = "Re-reads `saka.toml` at the tip of the base branch and applies it; exit 1 if refused"
    )]
    Reload {
        #[arg(help = "Project id or name")]
        project: String,
    },
    #[command(
        about = "Prints who authors the project's agent changes; with --mode, changes it (exit 4 if refused)"
    )]
    Authorship {
        #[arg(help = "Project id or name")]
        project: String,
        #[arg(
            help = "bot: Saka; user: the linked account; user-pat: the owner of the project's user token",
            long,
            value_enum
        )]
        mode: Option<AuthorshipMode>,
    },
    #[command(
        subcommand,
        about = "Stores or removes the project's fine-grained user token"
    )]
    UserToken(UserTokenCmd),
}

#[derive(Subcommand)]
enum UserTokenCmd {
    #[command(
        about = "Checks and stores a fine-grained token (github_pat_...); one line is read from stdin, never from the command line"
    )]
    Set {
        #[arg(help = "Project id or name")]
        project: String,
    },
    #[command(about = "Removes the project's user token; refused while the project uses it")]
    Rm {
        #[arg(help = "Project id or name")]
        project: String,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum AuthorshipMode {
    Bot,
    User,
    UserPat,
}

impl AuthorshipMode {
    fn wire(self) -> &'static str {
        match self {
            AuthorshipMode::Bot => "bot",
            AuthorshipMode::User => "user",
            AuthorshipMode::UserPat => "user_pat",
        }
    }
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
    #[command(about = "Withdraws a spec that has no active tasks")]
    Withdraw {
        #[arg(help = "Project id or name")]
        project: String,
        #[arg(help = "Path of the spec file from the repo root")]
        path: String,
        #[arg(long)]
        reason: String,
    },
    #[command(
        about = "Asks the architect to draft a new spec from an idea; prints the draft id",
        after_help = "Exit codes with --wait: 0 succeeded, 1 failed, 3 failed on lint, 4 cancelled."
    )]
    Draft {
        #[arg(help = "Project id or name")]
        project: String,
        #[arg(help = "Path of the new spec file from the repo root")]
        path: String,
        #[arg(
            help = "The idea, in your own words",
            long,
            required_unless_present = "idea_file",
            conflicts_with = "idea_file"
        )]
        idea: Option<String>,
        #[arg(help = "Read the idea from this file", long)]
        idea_file: Option<PathBuf>,
        #[arg(
            help = "A doc in the project folder the architect reads first (repeatable, up to 16)",
            long = "context"
        )]
        context: Vec<String>,
        #[arg(
            help = "Language of the spec: en or id (default en)",
            long,
            value_parser = ["en", "id"]
        )]
        locale: Option<String>,
        #[arg(help = "Wait until the draft succeeds, fails, or is cancelled", long)]
        wait: bool,
    },
    #[command(about = "Prints a spec template that passes lint")]
    Template {
        #[arg(long, default_value = "en", help = "Template language: en or id")]
        locale: String,
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
    #[command(about = "Sends a message to the task's running or idle agent session")]
    Steer { id: String, text: String },
    #[command(about = "Stops the agent's current turn; the session waits for a steer")]
    Interrupt { id: String },
    #[command(about = "Ends the task's agent session and blocks the task")]
    Stop {
        id: String,
        #[arg(long)]
        reason: String,
    },
    #[command(about = "Prints what the task's agent sessions did, in order")]
    Timeline { id: String },
}

#[derive(Subcommand)]
enum DraftCmd {
    #[command(about = "Lists the project's spec drafts")]
    List {
        #[arg(help = "Project id or name")]
        project: String,
        #[arg(
            help = "active (default), finished, or all",
            long,
            default_value = "active",
            value_parser = ["active", "finished", "all"]
        )]
        state: String,
    },
    #[command(about = "Shows a draft: state, attempt, latest lint round, and failure")]
    Show { id: String },
    #[command(about = "Prints what the draft's latest architect session did, in order")]
    Timeline { id: String },
    #[command(about = "Sends a message to the draft's running or idle architect session")]
    Steer { id: String, text: String },
    #[command(about = "Stops the architect's current turn; the session waits for a steer")]
    Interrupt { id: String },
    #[command(about = "Ends the draft's architect session and fails the draft")]
    Stop {
        id: String,
        #[arg(long)]
        reason: String,
    },
    #[command(about = "Starts a new attempt of a failed draft")]
    Retry {
        id: String,
        #[arg(long)]
        reason: String,
        #[arg(help = "Sent to the architect with the new attempt", long)]
        note: Option<String>,
    },
    #[command(about = "Cancels a draft that is not committing")]
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
        #[arg(
            help = "With allow: allow the same request in this project from now on",
            long
        )]
        remember: bool,
    },
    #[command(about = "Lists the project's remembered permission rules")]
    Rules { project: String },
    #[command(about = "Revokes a remembered permission rule")]
    Revoke {
        id: String,
        #[arg(long)]
        reason: String,
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
        Command::Init(args) => init::run(&args, cli.server, cli.json),
        Command::Runner { session, launch } => {
            let code = current_thread()?.block_on(saka_agents::runner::run(&session, &launch));
            ExitCode::from(u8::try_from(code).unwrap_or(1))
        }
        Command::Secret(cmd) => {
            let client = Client::new(&server(cli.server)?, cli.json);
            current_thread()?.block_on(async {
                match cmd {
                    SecretCmd::Set {
                        project,
                        name,
                        multiline,
                    } => match cli::read_secret(&name, multiline) {
                        Ok(value) => client.secret_set(&project, &name, &value).await,
                        Err(code) => code,
                    },
                    SecretCmd::List { project } => client.secret_list(&project).await,
                    SecretCmd::Rm { project, name } => client.secret_rm(&project, &name).await,
                }
            })
        }
        Command::Scm(cmd) => {
            let given = match &cmd {
                ScmCmd::Link { integration, .. }
                | ScmCmd::Status { integration }
                | ScmCmd::Unlink { integration } => integration.clone(),
            };
            let integration = integration_of(given)?;
            let client = Client::new(&server(cli.server)?, cli.json);
            current_thread()?.block_on(async {
                match cmd {
                    ScmCmd::Link { no_wait, .. } => client.scm_link(&integration, !no_wait).await,
                    ScmCmd::Status { .. } => client.scm_status(&integration).await,
                    ScmCmd::Unlink { .. } => client.scm_unlink(&integration).await,
                }
            })
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
                    ProjectCmd::Reload { project } => client.project_reload(&project).await,
                    ProjectCmd::Authorship { project, mode } => {
                        client
                            .project_authorship(&project, mode.map(AuthorshipMode::wire))
                            .await
                    }
                    ProjectCmd::UserToken(UserTokenCmd::Set { project }) => {
                        match cli::read_secret("the user token", false) {
                            Ok(value) => client.user_token_set(&project, &value).await,
                            Err(code) => code,
                        }
                    }
                    ProjectCmd::UserToken(UserTokenCmd::Rm { project }) => {
                        client.user_token_rm(&project).await
                    }
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
        Command::Spec(SpecCmd::Withdraw {
            project,
            path,
            reason,
        }) => {
            let client = Client::new(&server(cli.server)?, cli.json);
            current_thread()?.block_on(client.spec_withdraw(&project, &path, &reason))
        }
        Command::Spec(SpecCmd::Draft {
            project,
            path,
            idea,
            idea_file,
            context,
            locale,
            wait,
        }) => {
            let idea = match (idea, idea_file) {
                (Some(text), _) => text,
                (None, Some(file)) => cli::read_idea(&file)?,
                (None, None) => String::new(),
            };
            let client = Client::new(&server(cli.server)?, cli.json);
            let new = cli::NewDraft {
                project: &project,
                path: &path,
                idea: &idea,
                context: &context,
                locale: locale.as_deref(),
                wait,
            };
            current_thread()?.block_on(client.spec_draft(&new))
        }
        Command::Draft(cmd) => {
            let client = Client::new(&server(cli.server)?, cli.json);
            current_thread()?.block_on(async {
                match cmd {
                    DraftCmd::List { project, state } => client.draft_list(&project, &state).await,
                    DraftCmd::Show { id } => client.draft_show(&id).await,
                    DraftCmd::Timeline { id } => client.draft_timeline(&id).await,
                    DraftCmd::Steer { id, text } => {
                        client
                            .draft_session(&id, "steer", &serde_json::json!({ "text": text }))
                            .await
                    }
                    DraftCmd::Interrupt { id } => {
                        client
                            .draft_session(&id, "interrupt", &serde_json::json!({}))
                            .await
                    }
                    DraftCmd::Stop { id, reason } => {
                        client
                            .draft_session(&id, "stop", &serde_json::json!({ "reason": reason }))
                            .await
                    }
                    DraftCmd::Retry { id, reason, note } => {
                        client
                            .draft_action(
                                &id,
                                "retry",
                                &serde_json::json!({ "reason": reason, "note": note }),
                            )
                            .await
                    }
                    DraftCmd::Cancel { id, reason } => {
                        client
                            .draft_action(&id, "cancel", &serde_json::json!({ "reason": reason }))
                            .await
                    }
                }
            })
        }
        Command::Spec(SpecCmd::Template { locale }) => {
            let client = Client::new(&server(cli.server)?, cli.json);
            current_thread()?.block_on(client.spec_template(&locale))
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
                    TaskCmd::Steer { id, text } => {
                        client
                            .task_session(&id, "steer", &serde_json::json!({ "text": text }))
                            .await
                    }
                    TaskCmd::Interrupt { id } => {
                        client
                            .task_session(&id, "interrupt", &serde_json::json!({}))
                            .await
                    }
                    TaskCmd::Stop { id, reason } => {
                        client
                            .task_session(&id, "stop", &serde_json::json!({ "reason": reason }))
                            .await
                    }
                    TaskCmd::Timeline { id } => client.task_timeline(&id).await,
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
                        remember,
                    } => {
                        let d = match decision {
                            Decision::Allow => "allow",
                            Decision::Deny => "deny",
                        };
                        client
                            .permission_answer(&id, d, message.as_deref(), remember)
                            .await
                    }
                    PermissionCmd::Rules { project } => client.permission_rules(&project).await,
                    PermissionCmd::Revoke { id, reason } => {
                        client.permission_revoke(&id, &reason).await
                    }
                }
            })
        }
    })
}
