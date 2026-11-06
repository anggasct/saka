mod agent_login;
mod cli;
mod cli_token;
mod config;
mod doctor;
mod init;
mod logging;
mod mcp;
mod notify_cmd;
mod scm_app;
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
    #[arg(
        help = "A file holding a device token (default: SAKA_TOKEN, then the local CLI token for a loopback server)",
        long,
        global = true
    )]
    token_file: Option<PathBuf>,
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
        about = "Serves MCP tools to one agent session over stdio (started by the agent CLI, not by hand)",
        hide = true,
        after_help = "Exit codes: 0 done, 2 the engine socket or the session token is missing or refused."
    )]
    Mcp {
        #[arg(long)]
        session: String,
    },
    #[command(
        about = "Sets up this repo for Saka: docs location, saka.toml, toolchain versions, and a setup recipe; registers the project once saka.toml is pushed",
        after_help = "Exit codes: 0 done, 1 failed, 2 usage or environment, 3 config problems, 5 waiting for a push. Saka never commits or pushes."
    )]
    Init(init::InitArgs),
    #[command(
        about = "Checks whether this instance can run the loop, with one fix per problem",
        after_help = "Exit codes: 0 no failures, 1 a failure (or a warning with --strict), 2 config refused or a flag is invalid. Uses the server's report; runs the local checks itself when the server is down."
    )]
    Doctor(doctor::DoctorArgs),
    #[command(
        about = "The agent CLI account Saka's sessions use (on the host that runs `saka serve`)"
    )]
    #[command(subcommand)]
    Agent(AgentCmd),
    #[command(about = "Projects")]
    #[command(subcommand)]
    Project(ProjectCmd),
    #[command(about = "Specs")]
    #[command(subcommand)]
    Spec(SpecCmd),
    #[command(about = "Tasks")]
    #[command(subcommand)]
    Task(TaskCmd),
    #[command(
        about = "Takes a pull request opened outside Saka and asks for its review at the current head",
        after_help = "Exit codes: 0 requested, 1 failed, 4 refused (already requested, not open, not allowed by the workflow, or a task of a spec step)."
    )]
    Review {
        #[arg(help = "The pull request number")]
        number: u64,
        #[arg(help = "Project id or name (default: the only project)", long)]
        project: Option<String>,
        #[arg(
            help = "Name of the repo binding in the project (default: its default repo)",
            long
        )]
        repo: Option<String>,
    },
    #[command(about = "Spec drafts the architect writes from an idea")]
    #[command(subcommand)]
    Draft(DraftCmd),
    #[command(about = "Permission requests from agent sessions")]
    #[command(subcommand)]
    Permission(PermissionCmd),
    #[command(about = "Questions agent sessions ask you, and your answers")]
    #[command(subcommand)]
    Decision(DecisionCmd),
    #[command(about = "Project secrets for environment recipes (write-only)")]
    #[command(subcommand)]
    Secret(SecretCmd),
    #[command(about = "SCM integrations: create the GitHub App, link your account")]
    #[command(subcommand)]
    Scm(ScmCmd),
    #[command(about = "Hooks of a project: what is configured, what ran, and run it again")]
    #[command(subcommand)]
    Hook(HookCmd),
    #[command(about = "Chat notification channels from the operator config")]
    #[command(subcommand)]
    Notify(NotifyCmd),
    #[command(about = "What waits for you across projects: permissions, blockers, approvals")]
    #[command(subcommand)]
    Inbox(InboxCmd),
    #[command(
        about = "Sign-in codes, device tokens, and revoking them (needs [auth] in the config)"
    )]
    #[command(subcommand)]
    Auth(AuthCmd),
}

#[derive(Subcommand)]
enum InboxCmd {
    #[command(
        about = "Lists what is waiting for you, newest first",
        after_help = "Exit codes: 0 ok, 1 failed, 3 problems."
    )]
    List {
        #[arg(help = "Only this project (id or name)", long)]
        project: Option<String>,
        #[arg(help = "How many items, 1 to 200", long, default_value_t = 50)]
        limit: u32,
    },
    #[command(
        about = "Dismisses a spec approval; it stays away until the spec has a new revision",
        after_help = "Exit codes: 0 dismissed, 1 failed, 3 problems, 4 the item cannot be dismissed or is closed."
    )]
    Dismiss {
        id: String,
        #[arg(long)]
        reason: String,
    },
}

#[derive(Subcommand)]
enum AuthCmd {
    #[command(
        about = "Creates a code that registers a passkey from the browser; run it on the host",
        after_help = "Exit codes: 0 created, 1 failed, 2 refused. The code is valid once for 15 minutes."
    )]
    Setup,
    #[command(
        about = "Creates a code that pairs another device (a token for a CLI or app, or a passkey with --passkey)",
        after_help = "Exit codes: 0 created, 1 failed. The code is valid once for 5 minutes."
    )]
    Pair {
        #[arg(
            help = "Pair a passkey from the browser instead of a device token",
            long
        )]
        passkey: bool,
    },
    #[command(
        about = "Turns a token code into a device token; run it on the device being paired",
        after_help = "Exit codes: 0 paired, 1 failed. The token is printed once, or written to --out with mode 0600."
    )]
    Redeem {
        code: String,
        #[arg(help = "A name for this device, 1 to 100 characters", long)]
        name: String,
        #[arg(help = "Write the token to this new file instead of printing it", long)]
        out: Option<PathBuf>,
    },
    #[command(about = "Lists paired devices and passkeys")]
    Devices,
    #[command(
        about = "Revokes a paired device or, with --passkey, a passkey and its sessions; it stops working at once",
        after_help = "Exit codes: 0 revoked, 1 failed, 4 the local CLI token or the last passkey cannot be revoked."
    )]
    Revoke {
        id: String,
        #[arg(help = "The id is a passkey, not a device", long)]
        passkey: bool,
    },
}

#[derive(Subcommand)]
enum NotifyCmd {
    #[command(
        about = "Sends a test message to a channel, to check the token and chat (runs here, not through the server)",
        after_help = "Exit codes: 0 sent, 1 the channel refused or could not be reached, 2 config refused or no such channel."
    )]
    Test {
        #[arg(help = "The channel name from [[notify.channel]]")]
        channel: String,
    },
}

#[derive(Subcommand)]
enum HookCmd {
    #[command(about = "Lists the hooks configured for a project in the operator config")]
    List {
        #[arg(help = "Project id or name")]
        project: String,
    },
    #[command(
        about = "Lists recent hook runs, newest first",
        after_help = "Exit codes: 0 ok, 1 failed, 3 problems."
    )]
    Runs {
        #[arg(help = "Project id or name")]
        project: String,
        #[arg(help = "Only runs of this hook", long)]
        hook: Option<String>,
        #[arg(help = "pending, succeeded, or failed", long)]
        state: Option<String>,
        #[arg(help = "How many runs, 1 to 200", long, default_value_t = 20)]
        limit: u32,
    },
    #[command(
        about = "Runs a finished hook run again with the hook's current definition",
        after_help = "Exit codes: 0 queued, 1 failed, 4 the run is pending or the hook is gone."
    )]
    Rerun {
        id: String,
        #[arg(long)]
        reason: String,
    },
}

#[derive(Subcommand)]
enum AgentCmd {
    #[command(
        about = "Logs the agent CLI in to Saka's account directory with its own login (never your personal config)",
        after_help = "Exit codes: 0 logged in, 1 not found or the login failed, 2 config refused."
    )]
    Login {
        #[arg(
            help = "Log in with an Anthropic Console account (API usage billing)",
            long
        )]
        console: bool,
        #[arg(help = "Log in again even when the account is logged in", long)]
        force: bool,
    },
    #[command(about = "Shows whether the agent account is logged in; exits 1 when it is not")]
    Status,
}

#[derive(Subcommand)]
enum ScmCmd {
    #[command(
        about = "Creates the GitHub App from a manifest on this host and adds it to the config as [[scm]] (no server needed)",
        after_help = "Exit codes: 0 created, 1 failed after the listener started, 2 refused before it started, 130 interrupted. On a headless host use --port <n> --no-browser and ssh -L <n>:127.0.0.1:<n>."
    )]
    CreateApp(scm_app::CreateAppArgs),
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
    #[command(
        about = "Starts a conversation with the architect about a spec; prints the draft id",
        after_help = "With --wait the command returns when the architect waits for your reply (0), or the draft fails (1, 3 on lint) or is cancelled (4). Reply with `saka draft message`, then `saka draft commit` or `saka draft discard`."
    )]
    Session {
        #[arg(help = "Project id or name")]
        project: String,
        #[arg(help = "Path of the spec file from the repo root")]
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
        #[arg(
            help = "Revise the spec file that already exists at the path instead of writing a new one",
            long
        )]
        revise: bool,
        #[arg(help = "Wait until the architect waits for your reply", long)]
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
    #[command(about = "Approves a step that waits for you before it starts")]
    Start { id: String },
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
    #[command(
        about = "Takes the task over: the agent stops, the task pauses, and you work on its branch",
        after_help = "Exit codes: 0 taken over, 4 the task cannot be taken over now."
    )]
    Takeover {
        id: String,
        #[arg(long)]
        reason: String,
    },
    #[command(
        about = "Hands a taken over task back to the loop at the latest head of its change",
        after_help = "Exit codes: 0 returned, 4 the task is not taken over or its change is no longer open."
    )]
    Return {
        id: String,
        #[arg(
            help = "Stage to re-enter (default: the stage the task left)",
            long,
            value_parser = ["dev", "review", "fix", "merge"]
        )]
        stage: Option<String>,
        #[arg(long)]
        reason: String,
    },
    #[command(
        about = "Starts a fix round on a pull request from outside Saka whose review asked for changes",
        after_help = "Exit codes: 0 started, 1 failed, 4 refused (a fork, no findings at the current head, rounds spent, not in the workflow, or a task of a spec step)."
    )]
    Fix { id: String },
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
    #[command(about = "Replies to the architect of a conversation that waits for you")]
    Message { id: String, text: String },
    #[command(about = "Commits the files of a conversation in one commit and registers the spec")]
    Commit {
        id: String,
        #[arg(help = "The commit message (1 to 200 characters)", long)]
        message: Option<String>,
    },
    #[command(about = "Discards a conversation that waits for you")]
    Discard {
        id: String,
        #[arg(long)]
        reason: String,
    },
    #[command(
        about = "Lists the files a conversation changed with their diffs and the lint result"
    )]
    Changes {
        id: String,
        #[arg(help = "Print the diff of every file", long)]
        diff: bool,
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
enum DecisionCmd {
    #[command(
        about = "Lists the questions agents asked",
        after_help = "Exit codes: 0 ok, 1 failed, 3 problems."
    )]
    List {
        #[arg(
            default_value = "asked",
            help = "asked, answered, delivered or expired; comma-separated",
            long
        )]
        state: String,
    },
    #[command(
        about = "Shows a question with its numbered options; option 1 is the agent's recommendation",
        after_help = "Exit codes: 0 ok, 1 failed or not found, 3 problems."
    )]
    Show { id: String },
    #[command(
        about = "Answers a question; the agent gets the answer as its next message",
        after_help = "Exit codes: 0 answered, 1 failed or refused, 2 usage, 4 the question is already answered."
    )]
    Answer {
        id: String,
        #[arg(
            help = "The option you choose, numbered from 1 as `decision show` lists them",
            long,
            required_unless_present = "note",
            value_parser = clap::value_parser!(u32).range(1..)
        )]
        option: Option<u32>,
        #[arg(help = "Text for the agent, at most 2000 characters", long)]
        note: Option<String>,
    },
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
    #[command(about = "Permission rules of a project", subcommand)]
    Rule(RuleCmd),
}

#[derive(Clone, Copy, ValueEnum)]
enum RuleEffect {
    Allow,
    Deny,
    Ask,
}

#[derive(Clone, Copy, ValueEnum)]
enum RuleMatch {
    Exact,
    Glob,
}

#[derive(Subcommand)]
enum RuleCmd {
    #[command(
        about = "Adds a rule to the project",
        after_help = "Exit codes: 0 added, 1 failed or refused (too broad, invalid), 4 an identical rule is already active."
    )]
    Add {
        project: String,
        #[arg(help = "What the rule does", long, value_enum)]
        effect: RuleEffect,
        #[arg(help = "The agent CLI's tool name, e.g. Bash or Edit", long)]
        tool: String,
        #[arg(
            default_value = "exact",
            help = "exact compares verbatim, glob uses * and ?",
            long = "match",
            value_enum
        )]
        match_kind: RuleMatch,
        #[arg(help = "Required for Bash: one part of a compound command", long)]
        command: Option<String>,
        #[arg(help = "Worktree-relative path; not for Bash", long)]
        path: Option<String>,
        #[arg(help = "Why the rule exists, at most 500 characters", long)]
        note: Option<String>,
    },
    #[command(about = "Lists the project's active rules and the operator's rules")]
    List { project: String },
    #[command(
        about = "Revokes a project rule",
        after_help = "Exit codes: 0 revoked, 1 failed, 4 already revoked."
    )]
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

fn connect(
    server: String,
    json: bool,
    token_file: Option<&std::path::Path>,
) -> Result<Client, ExitCode> {
    let token = cli_token::resolve(&server, token_file)?;
    Ok(Client::new(&server, json).authorized(token.as_ref()))
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
        Command::Init(args) => init::run(&args, cli.server, cli.json, cli.token_file.as_deref()),
        Command::Doctor(args) => current_thread()?.block_on(doctor::run(
            &args,
            cli.server,
            cli.json,
            cli.token_file.as_deref(),
        )),
        Command::Agent(AgentCmd::Login { console, force }) => {
            agent_login::login(&load_config()?, console, force)
        }
        Command::Agent(AgentCmd::Status) => agent_login::status(&load_config()?, cli.json),
        Command::Runner { session, launch } => {
            let code = current_thread()?.block_on(saka_agents::runner::run(&session, &launch));
            ExitCode::from(u8::try_from(code).unwrap_or(1))
        }
        Command::Mcp { session } => current_thread()?.block_on(mcp::run(&session)),
        Command::Secret(cmd) => {
            let client = connect(server(cli.server)?, cli.json, cli.token_file.as_deref())?;
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
        Command::Auth(cmd) => {
            let client = connect(server(cli.server)?, cli.json, cli.token_file.as_deref())?;
            current_thread()?.block_on(async {
                match cmd {
                    AuthCmd::Setup => client.auth_pairing("setup").await,
                    AuthCmd::Pair { passkey } => {
                        client
                            .auth_pairing(if passkey { "passkey" } else { "token" })
                            .await
                    }
                    AuthCmd::Redeem { code, name, out } => {
                        client.auth_redeem(&code, &name, out.as_deref()).await
                    }
                    AuthCmd::Devices => client.auth_devices().await,
                    AuthCmd::Revoke { id, passkey } => client.auth_revoke(&id, passkey).await,
                }
            })
        }
        Command::Notify(NotifyCmd::Test { channel }) => {
            notify_cmd::test(&load_config()?, &channel, cli.json)
        }
        Command::Hook(cmd) => {
            let client = connect(server(cli.server)?, cli.json, cli.token_file.as_deref())?;
            current_thread()?.block_on(async {
                match cmd {
                    HookCmd::List { project } => client.hook_list(&project).await,
                    HookCmd::Runs {
                        project,
                        hook,
                        state,
                        limit,
                    } => {
                        client
                            .hook_runs(&project, hook.as_deref(), state.as_deref(), limit)
                            .await
                    }
                    HookCmd::Rerun { id, reason } => client.hook_rerun(&id, &reason).await,
                }
            })
        }
        Command::Inbox(cmd) => {
            let client = Client::new(&server(cli.server)?, cli.json);
            current_thread()?.block_on(async {
                match cmd {
                    InboxCmd::List { project, limit } => {
                        client.inbox_list(project.as_deref(), limit).await
                    }
                    InboxCmd::Dismiss { id, reason } => client.inbox_dismiss(&id, &reason).await,
                }
            })
        }
        Command::Scm(ScmCmd::CreateApp(args)) => scm_app::run(&args, cli.json),
        Command::Scm(cmd) => {
            let given = match &cmd {
                ScmCmd::Link { integration, .. }
                | ScmCmd::Status { integration }
                | ScmCmd::Unlink { integration } => integration.clone(),
                ScmCmd::CreateApp(_) => None,
            };
            let integration = integration_of(given)?;
            let client = connect(server(cli.server)?, cli.json, cli.token_file.as_deref())?;
            current_thread()?.block_on(async {
                match cmd {
                    ScmCmd::Link { no_wait, .. } => client.scm_link(&integration, !no_wait).await,
                    ScmCmd::Status { .. } => client.scm_status(&integration).await,
                    ScmCmd::Unlink { .. } => client.scm_unlink(&integration).await,
                    ScmCmd::CreateApp(_) => ExitCode::from(serve::EXIT_REFUSED),
                }
            })
        }
        Command::Project(cmd) => {
            let client = connect(server(cli.server)?, cli.json, cli.token_file.as_deref())?;
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
            let client = connect(server(cli.server)?, cli.json, cli.token_file.as_deref())?;
            current_thread()?.block_on(client.spec_approve(&project, &path, reason.as_deref()))
        }
        Command::Spec(SpecCmd::Withdraw {
            project,
            path,
            reason,
        }) => {
            let client = connect(server(cli.server)?, cli.json, cli.token_file.as_deref())?;
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
            let client = connect(server(cli.server)?, cli.json, cli.token_file.as_deref())?;
            let new = cli::NewDraft {
                project: &project,
                path: &path,
                idea: &idea,
                context: &context,
                locale: locale.as_deref(),
                session: false,
                revise: false,
                wait,
            };
            current_thread()?.block_on(client.spec_draft(&new))
        }
        Command::Spec(SpecCmd::Session {
            project,
            path,
            idea,
            idea_file,
            context,
            locale,
            revise,
            wait,
        }) => {
            let idea = match (idea, idea_file) {
                (Some(text), _) => text,
                (None, Some(file)) => cli::read_idea(&file)?,
                (None, None) => String::new(),
            };
            let client = connect(server(cli.server)?, cli.json, cli.token_file.as_deref())?;
            let new = cli::NewDraft {
                project: &project,
                path: &path,
                idea: &idea,
                context: &context,
                locale: locale.as_deref(),
                session: true,
                revise,
                wait,
            };
            current_thread()?.block_on(client.spec_draft(&new))
        }
        Command::Draft(cmd) => {
            let client = connect(server(cli.server)?, cli.json, cli.token_file.as_deref())?;
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
                    DraftCmd::Message { id, text } => {
                        client
                            .draft_operator(&id, "message", &serde_json::json!({ "text": text }))
                            .await
                    }
                    DraftCmd::Commit { id, message } => {
                        client
                            .draft_operator(
                                &id,
                                "commit",
                                &serde_json::json!({ "message": message }),
                            )
                            .await
                    }
                    DraftCmd::Discard { id, reason } => {
                        client
                            .draft_operator(
                                &id,
                                "discard",
                                &serde_json::json!({ "reason": reason }),
                            )
                            .await
                    }
                    DraftCmd::Changes { id, diff } => client.draft_changes(&id, diff).await,
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
            let client = connect(server(cli.server)?, cli.json, cli.token_file.as_deref())?;
            current_thread()?.block_on(client.spec_template(&locale))
        }
        Command::Review {
            number,
            project,
            repo,
        } => {
            let client = connect(server(cli.server)?, cli.json, cli.token_file.as_deref())?;
            current_thread()?.block_on(client.review(number, project.as_deref(), repo.as_deref()))
        }
        Command::Task(cmd) => {
            let client = connect(server(cli.server)?, cli.json, cli.token_file.as_deref())?;
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
                    TaskCmd::Fix { id } => client.task_fix(&id).await,
                    TaskCmd::Start { id } => client.task_start(&id).await,
                    TaskCmd::Retry { id, reason } => {
                        client.task_action(&id, "retry", &reason).await
                    }
                    TaskCmd::Cancel { id, reason } => {
                        client.task_action(&id, "cancel", &reason).await
                    }
                    TaskCmd::Takeover { id, reason } => {
                        client.task_action(&id, "takeover", &reason).await
                    }
                    TaskCmd::Return { id, stage, reason } => {
                        client.task_return(&id, stage.as_deref(), &reason).await
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
        Command::Decision(cmd) => {
            let client = connect(server(cli.server)?, cli.json, cli.token_file.as_deref())?;
            current_thread()?.block_on(async {
                match cmd {
                    DecisionCmd::List { state } => client.decision_list(&state).await,
                    DecisionCmd::Show { id } => client.decision_show(&id).await,
                    DecisionCmd::Answer { id, option, note } => {
                        client
                            .decision_answer(&id, option.map(|n| n - 1), note.as_deref())
                            .await
                    }
                }
            })
        }
        Command::Permission(cmd) => {
            let client = connect(server(cli.server)?, cli.json, cli.token_file.as_deref())?;
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
                    PermissionCmd::Rule(RuleCmd::List { project }) => {
                        client.permission_rules(&project).await
                    }
                    PermissionCmd::Rule(RuleCmd::Revoke { id, reason }) => {
                        client.permission_revoke(&id, &reason).await
                    }
                    PermissionCmd::Rule(RuleCmd::Add {
                        project,
                        effect,
                        tool,
                        match_kind,
                        command,
                        path,
                        note,
                    }) => {
                        let effect = match effect {
                            RuleEffect::Allow => "allow",
                            RuleEffect::Deny => "deny",
                            RuleEffect::Ask => "ask",
                        };
                        let kind = match match_kind {
                            RuleMatch::Exact => "exact",
                            RuleMatch::Glob => "glob",
                        };
                        client
                            .permission_rule_add(cli::NewRule {
                                project: &project,
                                effect,
                                tool: &tool,
                                match_kind: kind,
                                command: command.as_deref(),
                                path: path.as_deref(),
                                note: note.as_deref(),
                            })
                            .await
                    }
                }
            })
        }
    })
}
