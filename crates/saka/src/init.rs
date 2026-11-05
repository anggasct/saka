mod files;
mod git;
mod prompt;

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::ValueEnum;
use saka_core::paths;
use saka_engine::project_config;
use saka_engine::recipe::{self, RECIPE_PATH, RecipeAction, RecipeProposal};
use saka_engine::versions::{
    self, FileSkipped, Proposal, ProposalAction, ToolReport, VersionStatus,
};
use saka_env::HttpReleaseIndex;
use serde::Serialize;
use serde_json::{Value, json};

use crate::cli::{Client, EXIT_PENDING, EXIT_PROBLEMS};
use crate::config::Config;
use files::{Binding, ConfigAction, Note, Planned};
use prompt::Prompter;

pub const EXIT_USAGE: u8 = crate::serve::EXIT_REFUSED;
const EXIT_FAILED: u8 = 1;
const DEFAULT_DOCS_PATH: &str = "docs";
const MAX_PROJECT_NAME: usize = 80;
const MAX_REPO_NAME: usize = 32;

#[derive(Debug, Clone, PartialEq)]
pub struct InitError {
    pub exit: u8,
    pub code: &'static str,
    pub params: Value,
}

impl InitError {
    pub fn new(exit: u8, code: &'static str, params: Value) -> InitError {
        InitError { exit, code, params }
    }

    pub fn usage(code: &'static str, params: Value) -> InitError {
        InitError::new(EXIT_USAGE, code, params)
    }

    pub fn failed(code: &'static str, params: Value) -> InitError {
        InitError::new(EXIT_FAILED, code, params)
    }

    fn flag(flag: &str, value: &str) -> InitError {
        InitError::usage("flag_invalid", json!({ "flag": flag, "value": value }))
    }

    fn to_json(&self) -> Value {
        json!({ "code": self.code, "params": self.params })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DocsLocation {
    Here,
    Repo,
}

#[derive(clap::Args)]
pub struct InitArgs {
    #[arg(
        help = "The git remote of the code repo (and of the docs repo)",
        long,
        default_value = "origin"
    )]
    remote: String,
    #[arg(
        help = "Where the docs live: here (this repo) or repo (a separate docs repo)",
        long,
        value_enum
    )]
    docs: Option<DocsLocation>,
    #[arg(help = "Local clone of the docs repo (with --docs repo)", long)]
    docs_repo: Option<PathBuf>,
    #[arg(
        help = "Project folder in the docs repo (default: docs here, the code repo's name in a docs repo)",
        long
    )]
    docs_path: Option<String>,
    #[arg(
        help = "Branch of the docs repo Saka reads (default: its default branch)",
        long
    )]
    docs_base_branch: Option<String>,
    #[arg(help = "Name of a new project (default: the code repo's name)", long)]
    project_name: Option<String>,
    #[arg(
        help = "Name of this repo's [[repo]] entry (default: from the repo name)",
        long
    )]
    repo_name: Option<String>,
    #[arg(help = "Base branch written for this repo (default: none)", long)]
    base_branch: Option<String>,
    #[arg(
        help = "Path scope pattern for this repo (repeatable)",
        long = "path-scope"
    )]
    path_scope: Vec<String>,
    #[arg(help = "Make this repo the default repo of an existing project", long)]
    default: bool,
    #[arg(help = "Write the proposed version files without asking", long)]
    write_version_files: bool,
    #[arg(
        help = "Write the proposed .saka/environment.toml without asking",
        long
    )]
    write_recipe: bool,
    #[arg(help = "Do not call the server", long)]
    no_register: bool,
    #[arg(help = "Write nothing; print the report and the plan", long)]
    dry_run: bool,
    #[arg(help = "Do not ask; take every choice from flags and defaults", long)]
    yes: bool,
}

#[derive(Debug, Serialize)]
struct RepoReport {
    root: String,
    url: String,
    default_branch: Option<String>,
    name: String,
}

#[derive(Debug, Serialize)]
struct DocsReport {
    location: DocsLocation,
    root: String,
    workspace_url: String,
    project_path: String,
    base_branch: Option<String>,
}

#[derive(Debug, Serialize)]
struct ConfigReport {
    path: String,
    action: ConfigAction,
    written: bool,
    sha256: String,
    repo_name: String,
    notes: Vec<Note>,
}

#[derive(Debug, Serialize)]
struct RecipeReport {
    path: &'static str,
    action: RecipeAction,
    reason: Option<&'static str>,
    written: bool,
    commands: Vec<String>,
    issues: Vec<project_config::Issue>,
    notes: Vec<recipe::Note>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Outcome {
    Registered,
    Reloaded,
    UpToDate,
    PendingPush,
    Skipped,
    Failed,
}

#[derive(Debug, Serialize)]
struct Registration {
    outcome: Outcome,
    project_id: Option<String>,
    reload_outcome: Option<String>,
}

#[derive(Debug, Serialize)]
struct Pending {
    root: String,
    paths: Vec<String>,
}

#[derive(Debug, Serialize)]
struct InitReport {
    dry_run: bool,
    repo: RepoReport,
    docs: DocsReport,
    config: ConfigReport,
    toolchains: Vec<ToolReport>,
    files_skipped: Vec<FileSkipped>,
    recipe: RecipeReport,
    check: Option<Value>,
    registration: Registration,
    pending_files: Vec<Pending>,
    error: Option<Value>,
    exit_code: u8,
    #[serde(skip)]
    project_name: Option<String>,
}

fn display(path: &Path) -> String {
    path.display().to_string()
}

fn project_name_ok(name: &str) -> bool {
    (1..=MAX_PROJECT_NAME).contains(&name.chars().count()) && name.trim() == name
}

fn repo_name_ok(name: &str) -> bool {
    (1..=MAX_REPO_NAME).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

pub fn repo_name_from(name: &str) -> String {
    let mapped: String = name
        .to_ascii_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_lowercase() || c.is_ascii_digit() {
                c
            } else {
                '-'
            }
        })
        .collect();
    let cut: String = mapped
        .trim_matches('-')
        .chars()
        .take(MAX_REPO_NAME)
        .collect();
    let cut = cut.trim_end_matches('-').to_string();
    if cut.is_empty() {
        "repo".to_string()
    } else {
        cut
    }
}

fn remote_ok(remote: &str) -> bool {
    !remote.is_empty()
        && !remote.starts_with('-')
        && remote
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'/'))
}

fn validate(args: &InitArgs) -> Result<(), InitError> {
    if !remote_ok(&args.remote) {
        return Err(InitError::flag("--remote", &args.remote));
    }
    if let Some(p) = &args.docs_path
        && paths::check_project_path(p).is_err()
    {
        return Err(InitError::flag("--docs-path", p));
    }
    for (flag, branch) in [
        ("--docs-base-branch", &args.docs_base_branch),
        ("--base-branch", &args.base_branch),
    ] {
        if let Some(b) = branch
            && !saka_engine::branch_ok(b)
        {
            return Err(InitError::flag(flag, b));
        }
    }
    if let Some(n) = &args.project_name
        && !project_name_ok(n)
    {
        return Err(InitError::flag("--project-name", n));
    }
    if let Some(n) = &args.repo_name
        && !repo_name_ok(n)
    {
        return Err(InitError::flag("--repo-name", n));
    }
    if args.docs_repo.is_some() && args.docs == Some(DocsLocation::Here) {
        return Err(InitError::flag("--docs-repo", "with --docs here"));
    }
    Ok(())
}

pub fn ask_docs_location(p: &mut dyn Prompter) -> Result<DocsLocation, InitError> {
    prompt::ask(
        p,
        "Where do the docs live? [1] this repo (docs/)  [2] a separate docs repo",
        Some(DocsLocation::Here),
        |a| match a {
            "1" => Some(DocsLocation::Here),
            "2" => Some(DocsLocation::Repo),
            _ => None,
        },
    )
}

pub fn ask_docs_repo(p: &mut dyn Prompter) -> Result<PathBuf, InitError> {
    prompt::ask(p, "Path of the local clone of the docs repo:", None, |a| {
        Some(PathBuf::from(a))
    })
}

pub fn ask_docs_path(p: &mut dyn Prompter, default: &str) -> Result<String, InitError> {
    prompt::ask(
        p,
        &format!("Project folder in that repo [{default}]:"),
        Some(default.to_string()),
        |a| paths::check_project_path(a).is_ok().then(|| a.to_string()),
    )
}

pub fn ask_project_name(p: &mut dyn Prompter, default: &str) -> Result<String, InitError> {
    prompt::ask(
        p,
        &format!("Project name [{default}]:"),
        Some(default.to_string()),
        |a| project_name_ok(a).then(|| a.to_string()),
    )
}

pub fn ask_version_file(p: &mut dyn Prompter, report: &ToolReport) -> Result<bool, InitError> {
    let proposal = &report.proposal;
    prompt::confirm(
        p,
        &format!(
            "Write {} ({} {})? [Y/n]",
            proposal.path.as_deref().unwrap_or_default(),
            report.tool.as_str(),
            proposal.version.as_deref().unwrap_or_default()
        ),
    )
}

pub fn ask_recipe(p: &mut dyn Prompter, commands: &[String]) -> Result<bool, InitError> {
    prompt::confirm(
        p,
        &format!(
            "Write .saka/environment.toml with: {}? [Y/n]",
            commands.join("; ")
        ),
    )
}

struct Docs {
    location: DocsLocation,
    root: PathBuf,
    url: String,
    project_path: String,
}

struct Code {
    root: PathBuf,
    url: String,
    name: String,
    default_branch: Option<String>,
}

fn read_config_file(path: &Path) -> Result<Option<String>, InitError> {
    let bytes = files::read_regular(
        path,
        usize::try_from(saka_engine::MAX_CONFIG_BYTES).unwrap_or(usize::MAX),
    )
    .map_err(|e| {
        InitError::failed(
            "file_read_failed",
            json!({ "path": display(path), "reason": e.to_string() }),
        )
    })?;
    let Some(bytes) = bytes else {
        return Ok(None);
    };
    if bytes.len() as u64 > saka_engine::MAX_CONFIG_BYTES {
        return Err(InitError::new(
            EXIT_PROBLEMS,
            "existing_config_invalid",
            json!({ "issues": [{ "code": "toml_invalid", "field": null, "line": null }] }),
        ));
    }
    String::from_utf8(bytes).map(Some).map_err(|_| {
        InitError::new(
            EXIT_PROBLEMS,
            "existing_config_invalid",
            json!({ "issues": [{ "code": "toml_invalid", "field": null, "line": null }] }),
        )
    })
}

struct Session<'a, 'p> {
    args: &'a InitArgs,
    prompter: Option<&'p mut dyn Prompter>,
}

impl Session<'_, '_> {
    async fn docs(&mut self, code: &Code) -> Result<Docs, InitError> {
        let location = match (self.args.docs, &mut self.prompter) {
            (Some(l), _) => l,
            (None, _) if self.args.docs_repo.is_some() => DocsLocation::Repo,
            (None, Some(p)) => ask_docs_location(&mut **p)?,
            (None, None) => DocsLocation::Here,
        };
        if location == DocsLocation::Here {
            return Ok(Docs {
                location,
                root: code.root.clone(),
                url: code.url.clone(),
                project_path: self
                    .args
                    .docs_path
                    .clone()
                    .unwrap_or_else(|| DEFAULT_DOCS_PATH.to_string()),
            });
        }
        let dir = match (&self.args.docs_repo, &mut self.prompter) {
            (Some(d), _) => d.clone(),
            (None, Some(p)) => ask_docs_repo(&mut **p)?,
            (None, None) => return Err(InitError::usage("docs_repo_required", json!({}))),
        };
        let (root, repo, _) = git::discover(&dir, &self.args.remote).await?;
        if repo.url == code.url {
            return Ok(Docs {
                location: DocsLocation::Here,
                root: code.root.clone(),
                url: code.url.clone(),
                project_path: self
                    .args
                    .docs_path
                    .clone()
                    .unwrap_or_else(|| DEFAULT_DOCS_PATH.to_string()),
            });
        }
        let project_path = match (&self.args.docs_path, &mut self.prompter) {
            (Some(p), _) => p.clone(),
            (None, Some(p)) => ask_docs_path(&mut **p, &code.name)?,
            (None, None) => code.name.clone(),
        };
        if paths::check_project_path(&project_path).is_err() {
            return Err(InitError::flag("--docs-path", &project_path));
        }
        Ok(Docs {
            location,
            root,
            url: repo.url,
            project_path,
        })
    }

    fn plan(
        &mut self,
        code: &Code,
        existing: Option<&str>,
    ) -> Result<(Planned, Option<String>), InitError> {
        let binding = Binding {
            name: self
                .args
                .repo_name
                .clone()
                .unwrap_or_else(|| repo_name_from(&code.name)),
            url: code.url.clone(),
            base_branch: self.args.base_branch.clone(),
            path_scope: self.args.path_scope.clone(),
            make_default: self.args.default,
        };
        match existing {
            Some(text) => Ok((files::merge(text, &binding)?, None)),
            None => {
                if self.args.default {
                    return Err(InitError::flag(
                        "--default",
                        "without an existing saka.toml",
                    ));
                }
                let name = match (&self.args.project_name, &mut self.prompter) {
                    (Some(n), _) => n.clone(),
                    (None, Some(p)) => ask_project_name(&mut **p, &code.name)?,
                    (None, None) => code.name.clone(),
                };
                if !project_name_ok(&name) {
                    return Err(InitError::flag("--project-name", &name));
                }
                Ok((files::create(&name, &binding)?, Some(name)))
            }
        }
    }

    fn approve_versions(&mut self, tools: &[ToolReport]) -> Result<Vec<bool>, InitError> {
        tools
            .iter()
            .map(|t| {
                if !t.proposal.writes_file() {
                    return Ok(false);
                }
                match &mut self.prompter {
                    _ if self.args.write_version_files => Ok(true),
                    Some(p) => ask_version_file(&mut **p, t),
                    None => Ok(false),
                }
            })
            .collect()
    }

    fn approve_recipe(&mut self, proposal: &RecipeProposal) -> Result<bool, InitError> {
        if proposal.action != RecipeAction::Create {
            return Ok(false);
        }
        match &mut self.prompter {
            _ if self.args.write_recipe => Ok(true),
            Some(p) => ask_recipe(&mut **p, &proposal.commands),
            None => Ok(false),
        }
    }
}

fn write_version(root: &Path, proposal: &mut Proposal) -> Result<(), InitError> {
    let (Some(path), Some(content)) = (proposal.path.clone(), proposal.content.clone()) else {
        return Ok(());
    };
    let replace = proposal.action == ProposalAction::ReplaceFile;
    if replace {
        let current = files::read_regular(&root.join(&path), versions::MAX_FILE_BYTES)
            .ok()
            .flatten();
        if !current.as_deref().is_some_and(files::single_line) {
            proposal.params = json!({ "kept": "not_single_line" });
            return Ok(());
        }
    }
    files::write_atomic(root, &path, content.as_bytes(), replace)?;
    proposal.written = true;
    Ok(())
}

fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .map(display)
        .unwrap_or_else(|_| display(path))
}

struct Prepared {
    code: Code,
    docs: Docs,
    config_path: PathBuf,
    planned: Planned,
    project_name: Option<String>,
    config_written: bool,
    versions: versions::VersionReport,
    recipe: RecipeProposal,
    recipe_written: bool,
}

async fn prepare(
    args: &InitArgs,
    cfg: &Config,
    prompter: Option<&mut dyn Prompter>,
) -> Result<Prepared, InitError> {
    validate(args)?;
    let cwd = std::env::current_dir()
        .map_err(|e| InitError::usage("not_a_git_repo", json!({ "reason": e.to_string() })))?;
    let (root, repo, default_branch) = git::discover(&cwd, &args.remote).await?;
    let code = Code {
        root,
        url: repo.url,
        name: repo.name,
        default_branch,
    };
    let mut session = Session { args, prompter };
    let docs = session.docs(&code).await?;
    let config_rel = paths::project_config_path(&docs.project_path);
    let config_path = files::target(&docs.root, &config_rel)?;
    let existing = read_config_file(&config_path)?;
    let (planned, project_name) = session.plan(&code, existing.as_deref())?;

    let repo_files = files::collect(&code.root);
    let index = HttpReleaseIndex::new(crate::serve::release_index_config(cfg))
        .map_err(|e| InitError::usage("config_refused", json!({ "code": e.code })))?;
    let mut versions = versions::detect(&repo_files, &index).await;
    let recipe = recipe::propose(&repo_files, cfg.env.ports_per_env);

    let version_approvals = session.approve_versions(&versions.toolchains)?;
    let recipe_approved = session.approve_recipe(&recipe)?;

    let mut config_written = false;
    let mut recipe_written = false;
    if !args.dry_run {
        if planned.action != ConfigAction::None {
            files::write_atomic(
                &docs.root,
                &config_rel,
                planned.content.as_bytes(),
                planned.action == ConfigAction::Update,
            )?;
            config_written = true;
        }
        for (tool, approved) in versions.toolchains.iter_mut().zip(version_approvals) {
            if approved {
                write_version(&code.root, &mut tool.proposal)?;
            }
        }
        if recipe_approved && let Some(content) = &recipe.content {
            files::write_atomic(&code.root, RECIPE_PATH, content.as_bytes(), false)?;
            recipe_written = true;
        }
    }
    Ok(Prepared {
        code,
        docs,
        config_path,
        planned,
        project_name,
        config_written,
        versions,
        recipe,
        recipe_written,
    })
}

async fn pending_files(p: &Prepared) -> Result<Vec<Pending>, InitError> {
    let config_rel = relative(&p.docs.root, &p.config_path);
    let mut code_paths: Vec<String> = p
        .versions
        .toolchains
        .iter()
        .filter(|t| t.proposal.written)
        .filter_map(|t| t.proposal.path.clone())
        .collect();
    code_paths.push(RECIPE_PATH.to_string());
    let mut roots = vec![(p.code.root.clone(), code_paths)];
    if p.docs.root == p.code.root {
        roots[0].1.push(config_rel);
    } else {
        roots.push((p.docs.root.clone(), vec![config_rel]));
    }
    let mut out = Vec::new();
    for (root, paths) in roots {
        let found = git::pending(&root, &paths).await?;
        if !found.is_empty() {
            out.push(Pending {
                root: display(&root),
                paths: found,
            });
        }
    }
    Ok(out)
}

struct Server<'a> {
    client: &'a Client,
}

impl Server<'_> {
    async fn post(&self, path: &str, body: &Value) -> Result<Value, InitError> {
        let answer = self.client.post_quiet(path, body).await.map_err(|reason| {
            InitError::failed(
                "server_unreachable",
                json!({ "server": self.client.base(), "reason": reason }),
            )
        })?;
        if (200..300).contains(&answer.status) {
            Ok(answer.body)
        } else {
            Err(InitError::failed(
                "server_refused",
                json!({ "status": answer.status, "code": answer.body["code"], "params": answer.body["params"] }),
            ))
        }
    }
}

async fn register(
    server: &Server<'_>,
    docs: &Docs,
    docs_base_branch: Option<&str>,
    check: &Value,
    dry_run: bool,
) -> Result<(Registration, u8), InitError> {
    let none = |outcome| Registration {
        outcome,
        project_id: None,
        reload_outcome: None,
    };
    if check["ok"] != true {
        return Ok((none(Outcome::Skipped), EXIT_PROBLEMS));
    }
    let matches = check["remote"]["matches"] == true;
    let project_id = check["project"]["id"].as_str().map(String::from);
    if dry_run {
        return Ok((
            Registration {
                project_id,
                ..none(Outcome::Skipped)
            },
            0,
        ));
    }
    if !matches {
        return Ok((
            Registration {
                project_id,
                ..none(Outcome::PendingPush)
            },
            EXIT_PENDING,
        ));
    }
    match project_id {
        None => {
            let body = json!({
                "workspace_url": docs.url,
                "project_path": docs.project_path,
                "base_branch": docs_base_branch,
            });
            let created = server.post("/api/v1/projects", &body).await?;
            Ok((
                Registration {
                    outcome: Outcome::Registered,
                    project_id: created["id"].as_str().map(String::from),
                    reload_outcome: None,
                },
                0,
            ))
        }
        Some(id) if check["project"]["config_sha256"] == check["remote"]["config_sha256"] => Ok((
            Registration {
                outcome: Outcome::UpToDate,
                project_id: Some(id),
                reload_outcome: None,
            },
            0,
        )),
        Some(id) => {
            let reloaded = server
                .post(&format!("/api/v1/projects/{id}/actions/reload"), &json!({}))
                .await?;
            Ok((
                Registration {
                    outcome: Outcome::Reloaded,
                    project_id: Some(id),
                    reload_outcome: reloaded["outcome"].as_str().map(String::from),
                },
                0,
            ))
        }
    }
}

async fn flow(
    args: &InitArgs,
    cfg: &Config,
    client: &Client,
    prompter: Option<&mut dyn Prompter>,
) -> Result<InitReport, InitError> {
    let prepared = prepare(args, cfg, prompter).await?;
    let pending = pending_files(&prepared).await?;
    let Prepared {
        code,
        docs,
        config_path,
        planned,
        project_name,
        config_written,
        versions,
        recipe,
        recipe_written,
    } = prepared;
    let mut report = InitReport {
        dry_run: args.dry_run,
        repo: RepoReport {
            root: display(&code.root),
            url: code.url.clone(),
            default_branch: code.default_branch.clone(),
            name: code.name.clone(),
        },
        docs: DocsReport {
            location: docs.location,
            root: display(&docs.root),
            workspace_url: docs.url.clone(),
            project_path: docs.project_path.clone(),
            base_branch: args.docs_base_branch.clone(),
        },
        config: ConfigReport {
            path: display(&config_path),
            action: planned.action,
            written: config_written,
            sha256: project_config::sha256(planned.content.as_bytes()),
            repo_name: planned.repo_name.clone(),
            notes: planned.notes.clone(),
        },
        toolchains: versions.toolchains,
        files_skipped: versions.files_skipped,
        recipe: RecipeReport {
            path: RECIPE_PATH,
            action: recipe.action,
            reason: recipe.reason,
            written: recipe_written,
            commands: recipe.commands,
            issues: recipe.issues,
            notes: recipe.notes,
        },
        check: None,
        registration: Registration {
            outcome: Outcome::Skipped,
            project_id: None,
            reload_outcome: None,
        },
        pending_files: pending,
        error: None,
        exit_code: 0,
        project_name,
    };
    if args.no_register {
        return Ok(report);
    }
    let server = Server { client };
    let body = json!({
        "workspace_url": docs.url,
        "project_path": docs.project_path,
        "base_branch": args.docs_base_branch,
        "content": planned.content,
    });
    let check = match server
        .post("/api/v1/projects/actions/check-config", &body)
        .await
    {
        Ok(check) => check,
        Err(e) => {
            report.registration.outcome = Outcome::Failed;
            report.exit_code = e.exit;
            report.error = Some(e.to_json());
            return Ok(report);
        }
    };
    let decided = register(
        &server,
        &docs,
        args.docs_base_branch.as_deref(),
        &check,
        args.dry_run,
    )
    .await;
    report.check = Some(check);
    match decided {
        Ok((registration, exit)) => {
            report.registration = registration;
            report.exit_code = exit;
        }
        Err(e) => {
            report.registration.outcome = Outcome::Failed;
            report.exit_code = e.exit;
            report.error = Some(e.to_json());
        }
    }
    Ok(report)
}

fn out(line: &str) {
    let _ = writeln!(std::io::stdout(), "{line}");
}

fn err(line: &str) {
    let _ = writeln!(std::io::stderr(), "{line}");
}

fn quoted(value: Option<&str>) -> String {
    serde_json::to_string(value.unwrap_or_default()).unwrap_or_default()
}

fn proposal_text(t: &ToolReport) -> Option<String> {
    let p = &t.proposal;
    let version = p.version.as_deref().unwrap_or_default();
    let path = p.path.as_deref().unwrap_or_default();
    match p.action {
        ProposalAction::None => None,
        ProposalAction::WriteFile | ProposalAction::ReplaceFile if p.written => {
            Some(format!("wrote {path} ({version})"))
        }
        ProposalAction::WriteFile | ProposalAction::ReplaceFile => {
            Some(format!("write {path} ({version})"))
        }
        ProposalAction::Manual => Some(match &p.command {
            Some(command) => command.clone(),
            None => format!("{} {}", p.code.unwrap_or_default(), p.params),
        }),
    }
}

fn toolchain_line(t: &ToolReport) -> String {
    let tool = t.tool.as_str();
    let provisioned = if t.provisioned_by_saka {
        ""
    } else {
        ", not provisioned by Saka"
    };
    let head = match t.status {
        VersionStatus::Pinned => format!(
            "{tool} {} ({}{provisioned})",
            t.version.as_deref().unwrap_or_default(),
            t.source.unwrap_or_default()
        ),
        VersionStatus::Unpinned => format!(
            "{tool} unpinned {} ({}{provisioned})",
            quoted(t.value.as_deref()),
            t.source.unwrap_or_default()
        ),
        VersionStatus::EvidenceOnly | VersionStatus::Undeclared => {
            let status = t.status.as_str();
            if provisioned.is_empty() {
                format!("{tool} {status}")
            } else {
                format!("{tool} {status} (not provisioned by Saka)")
            }
        }
    };
    match proposal_text(t) {
        Some(next) => format!("toolchain  {head} → {next}"),
        None => format!("toolchain  {head}"),
    }
}

fn print_human(report: &InitReport) {
    let repo = &report.repo;
    out(&format!(
        "repo  {} {} (default branch {})",
        repo.url,
        repo.root,
        repo.default_branch.as_deref().unwrap_or("unknown")
    ));
    let docs = &report.docs;
    let location = match docs.location {
        DocsLocation::Here => "here",
        DocsLocation::Repo => "repo",
    };
    out(&format!(
        "docs  {location} {} {}",
        docs.workspace_url, docs.project_path
    ));
    let config = &report.config;
    let action = match config.action {
        ConfigAction::Create => "create",
        ConfigAction::Update => "update",
        ConfigAction::None => "none",
    };
    let state = if config.written {
        "written"
    } else if config.action == ConfigAction::None {
        "unchanged"
    } else {
        "not written"
    };
    out(&format!(
        "saka.toml  {action} {} [[repo]] {} ({state})",
        config.path, config.repo_name
    ));
    for note in &config.notes {
        out(&format!("saka.toml  {} {}", note.code, note.params));
    }
    for t in &report.toolchains {
        out(&toolchain_line(t));
        for d in &t.drift {
            out(&format!(
                "drift  {} {} {}:{} {} (pinned {})",
                t.tool.as_str(),
                d.source,
                d.file,
                d.line,
                d.value,
                d.pinned
            ));
        }
    }
    for s in &report.files_skipped {
        out(&format!("skipped  {} {}", s.path, s.reason));
    }
    let recipe = &report.recipe;
    match recipe.action {
        RecipeAction::Create => out(&format!(
            "recipe  {} {}: {}",
            if recipe.written { "wrote" } else { "proposed" },
            recipe.path,
            recipe.commands.join("; ")
        )),
        RecipeAction::None => out(&format!(
            "recipe  none {}",
            recipe.reason.unwrap_or_default()
        )),
    }
    for issue in &recipe.issues {
        out(&format!("recipe  issue {}", issue.to_json()));
    }
    for note in &recipe.notes {
        out(&format!("recipe  {} {}", note.code, note.params));
    }
    match &report.check {
        None => out("check  skipped"),
        Some(check) if check["ok"] == true => out("check  ok"),
        Some(check) => {
            let problems = check["problems"].as_array().cloned().unwrap_or_default();
            out(&format!("check  {} problems", problems.len()));
            for p in &problems {
                out(&format!(
                    "problem  {} {}",
                    p["code"].as_str().unwrap_or_default(),
                    p["params"]
                ));
            }
            for hint in create_app_hints(&problems) {
                out(&hint);
            }
        }
    }
    if report.registration.outcome == Outcome::PendingPush {
        for pending in &report.pending_files {
            out(&format!(
                "next  git -C {root} add {paths} && git -C {root} commit -m \"chore: set up Saka\" && git -C {root} push",
                root = pending.root,
                paths = pending.paths.join(" ")
            ));
        }
        out("next  then run saka init again");
    }
    let name = report
        .check
        .as_ref()
        .and_then(|c| c["project"]["name"].as_str().map(String::from))
        .or_else(|| report.project_name.clone())
        .unwrap_or_default();
    let id = report
        .registration
        .project_id
        .as_deref()
        .unwrap_or_default();
    match report.registration.outcome {
        Outcome::Registered => out(&format!("project  {name} {id} registered")),
        Outcome::Reloaded => out(&format!(
            "project  {name} {id} reloaded ({})",
            report
                .registration
                .reload_outcome
                .as_deref()
                .unwrap_or_default()
        )),
        Outcome::UpToDate => out(&format!("project  {name} {id} up to date")),
        Outcome::PendingPush | Outcome::Skipped | Outcome::Failed => {}
    }
}

fn create_app_hints(problems: &[Value]) -> Vec<String> {
    let mut seen = std::collections::BTreeSet::new();
    problems
        .iter()
        .filter(|p| p["code"] == "scm_integration_unknown")
        .filter_map(|p| p["params"]["host"].as_str())
        .filter(|host| seen.insert(*host))
        .map(|host| match host {
            "github.com" => "next  saka scm create-app".to_string(),
            other => format!("next  saka scm create-app --web-url https://{other}"),
        })
        .collect()
}

fn print_error(e: &InitError, json_out: bool) {
    let doc = e.to_json().to_string();
    err(&doc);
    if json_out {
        out(&doc);
    }
}

pub fn run(args: &InitArgs, server: Option<String>, json_out: bool) -> ExitCode {
    let report = run_inner(args, server, json_out);
    match report {
        Ok(report) => {
            if json_out {
                out(&serde_json::to_string(&report).unwrap_or_default());
            } else {
                print_human(&report);
            }
            if let Some(e) = &report.error {
                err(&e.to_string());
            }
            ExitCode::from(report.exit_code)
        }
        Err(e) => {
            print_error(&e, json_out);
            ExitCode::from(e.exit)
        }
    }
}

fn run_inner(
    args: &InitArgs,
    server: Option<String>,
    json_out: bool,
) -> Result<InitReport, InitError> {
    let cfg = Config::load().map_err(|e| {
        InitError::usage(
            "config_refused",
            json!({ "code": e.code, "detail": e.detail }),
        )
    })?;
    let server = server.unwrap_or_else(|| format!("http://{}", cfg.server.listen));
    let client = Client::new(&server, json_out);
    let interactive =
        !args.yes && std::io::stdin().is_terminal() && std::io::stderr().is_terminal();
    let mut terminal = prompt::Terminal;
    let prompter: Option<&mut dyn Prompter> = if interactive {
        Some(&mut terminal)
    } else {
        None
    };
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| InitError::failed("runtime_failed", json!({ "reason": e.to_string() })))?;
    rt.block_on(flow(args, &cfg, &client, prompter))
}

#[cfg(test)]
mod tests {
    use super::*;
    use saka_engine::versions::{Proposal, VersionTool};

    #[test]
    fn the_default_repo_name_is_lowercase_dashes_and_at_most_32() {
        for (raw, want) in [
            ("shop", "shop"),
            ("My_Shop.Web", "my-shop-web"),
            ("--x--", "x"),
            ("___", "repo"),
            ("a".repeat(40).as_str(), "a".repeat(32).as_str()),
            (&format!("{}-b", "a".repeat(31)), &"a".repeat(31)),
        ] {
            assert_eq!(repo_name_from(raw), want, "{raw}");
            assert!(repo_name_ok(&repo_name_from(raw)));
        }
    }

    fn tool(path: &str) -> ToolReport {
        ToolReport {
            tool: VersionTool::Node,
            status: VersionStatus::Unpinned,
            version: None,
            source: Some("nvmrc"),
            value: Some("22".into()),
            provisioned_by_saka: true,
            evidence: Vec::new(),
            drift: Vec::new(),
            proposal: Proposal {
                action: ProposalAction::ReplaceFile,
                path: Some(path.into()),
                version: Some("22.14.0".into()),
                content: Some("22.14.0\n".into()),
                command: None,
                code: None,
                params: json!({}),
                written: false,
            },
        }
    }

    #[test]
    fn prompts_come_in_order_and_empty_answers_take_the_default() {
        let mut p = prompt::Scripted::new(&["2", "/src/docs", "", "", "", "n", "y"]);
        assert_eq!(ask_docs_location(&mut p).unwrap(), DocsLocation::Repo);
        assert_eq!(ask_docs_repo(&mut p).unwrap(), PathBuf::from("/src/docs"));
        assert_eq!(ask_docs_path(&mut p, "shop").unwrap(), "shop");
        assert_eq!(ask_project_name(&mut p, "shop").unwrap(), "shop");
        assert!(ask_version_file(&mut p, &tool(".nvmrc")).unwrap());
        assert!(!ask_version_file(&mut p, &tool(".python-version")).unwrap());
        assert!(
            ask_recipe(
                &mut p,
                &[
                    "pnpm install --frozen-lockfile".into(),
                    "go mod download".into()
                ]
            )
            .unwrap()
        );
        assert_eq!(
            p.asked,
            [
                "Where do the docs live? [1] this repo (docs/)  [2] a separate docs repo",
                "Path of the local clone of the docs repo:",
                "Project folder in that repo [shop]:",
                "Project name [shop]:",
                "Write .nvmrc (node 22.14.0)? [Y/n]",
                "Write .python-version (node 22.14.0)? [Y/n]",
                "Write .saka/environment.toml with: pnpm install --frozen-lockfile; go mod download? [Y/n]",
            ]
        );
    }

    #[test]
    fn invalid_answers_are_asked_again_and_three_refuse() {
        let mut p = prompt::Scripted::new(&["3", "two", "1"]);
        assert_eq!(ask_docs_location(&mut p).unwrap(), DocsLocation::Here);
        assert_eq!(p.asked.len(), 3);
        let mut p = prompt::Scripted::new(&["../x", "/abs", "a/../b"]);
        let e = ask_docs_path(&mut p, "shop").unwrap_err();
        assert_eq!((e.exit, e.code), (EXIT_USAGE, "prompt_invalid"));
        let mut p = prompt::Scripted::new(&[&"x".repeat(81), &"z".repeat(85), &"y".repeat(90)]);
        assert_eq!(
            ask_project_name(&mut p, "shop").unwrap_err().code,
            "prompt_invalid"
        );
    }

    #[test]
    fn each_unknown_scm_host_gets_one_create_app_hint() {
        let problems = [
            json!({ "code": "scm_integration_unknown", "params": { "host": "github.com", "repo": null } }),
            json!({ "code": "scm_integration_unknown", "params": { "host": "ghe.example.com", "repo": null } }),
            json!({ "code": "project_config_invalid", "params": {} }),
            json!({ "code": "scm_integration_unknown", "params": { "host": "github.com", "repo": "x" } }),
        ];
        assert_eq!(
            create_app_hints(&problems),
            [
                "next  saka scm create-app",
                "next  saka scm create-app --web-url https://ghe.example.com",
            ]
        );
    }

    #[test]
    fn toolchain_lines_have_a_fixed_shape() {
        let mut node = tool(".nvmrc");
        node.status = VersionStatus::Pinned;
        node.version = Some("22.14.0".into());
        node.proposal = Proposal {
            action: ProposalAction::None,
            ..node.proposal.clone()
        };
        assert_eq!(toolchain_line(&node), "toolchain  node 22.14.0 (nvmrc)");
        let pnpm = ToolReport {
            tool: VersionTool::Pnpm,
            source: Some("package_json_package_manager"),
            value: Some("9".into()),
            proposal: Proposal {
                action: ProposalAction::Manual,
                path: None,
                content: None,
                command: Some("npm pkg set packageManager=pnpm@9.15.9".into()),
                code: Some("set_package_manager"),
                ..tool(".nvmrc").proposal
            },
            ..tool(".nvmrc")
        };
        assert_eq!(
            toolchain_line(&pnpm),
            "toolchain  pnpm unpinned \"9\" (package_json_package_manager) → npm pkg set packageManager=pnpm@9.15.9"
        );
        let rust = ToolReport {
            tool: VersionTool::Rust,
            status: VersionStatus::Pinned,
            version: Some("1.99.1".into()),
            source: Some("rust_toolchain_toml"),
            provisioned_by_saka: false,
            proposal: Proposal {
                action: ProposalAction::None,
                ..tool(".nvmrc").proposal
            },
            ..tool(".nvmrc")
        };
        assert_eq!(
            toolchain_line(&rust),
            "toolchain  rust 1.99.1 (rust_toolchain_toml, not provisioned by Saka)"
        );
    }
}
