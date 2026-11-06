mod gate;
mod github;
mod outside;

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;

use crate::metadata;
use github::GitHub;

const TEMPLATE: &str = include_str!("e2e/fixture-spec.md.tmpl");
const CONTEXT_TEMPLATE: &str = "# End-to-end context {run_id}\n\ncontext-marker: {run_id}\n";
const RUN_LIMIT: Duration = Duration::from_secs(40 * 60);
const READY_LIMIT: Duration = Duration::from_secs(60);
const CLEANUP_LIMIT: Duration = Duration::from_secs(3 * 60);
const STOP_LIMIT: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_secs(1);
const CONVERSATION_LIMIT: Duration = Duration::from_secs(600);
const FIRST_LINT_REPLY_ROUND: u64 = 3;
const LINT_REPLIES: u64 = 2;
const DEFAULT_MODEL: &str = "haiku";
const SLOW_COMMAND: &str = "sleep 30";
const ADVANCE_LIMIT: Duration = Duration::from_secs(5 * 60);
const TOUCH_LIMIT: usize = 3;
const DRAFT_IDEA: &str = "Add a `tasks count` command to the CLI that prints how many tasks are open and how many are done, one number per line.";
const ARCHITECT_TOOLS: &[&str] = &[
    "Read",
    "Glob",
    "Grep",
    "LS",
    "TodoWrite",
    "Write",
    "Edit",
    "MultiEdit",
];
const DENY_MESSAGE: &str = "Not allowed in this run. Use one command per call, without &&, ;, |, or redirects. The Done when commands are allowed exactly as written in the spec.";

struct Settings {
    app_id: u64,
    key_file: PathBuf,
    repo: String,
    claude_dir: PathBuf,
    model: String,
}

fn required(name: &str) -> Result<String> {
    std::env::var(name)
        .ok()
        .filter(|v| !v.is_empty())
        .with_context(|| format!("{name} is not set (see the end-to-end section of docs/guide.md)"))
}

fn settings() -> Result<Settings> {
    let app_id = required("SAKA_E2E_GITHUB_APP_ID")?
        .parse()
        .context("SAKA_E2E_GITHUB_APP_ID is not a number")?;
    let key_file = PathBuf::from(required("SAKA_E2E_GITHUB_PRIVATE_KEY_FILE")?);
    let mode = std::fs::metadata(&key_file)
        .with_context(|| format!("read {}", key_file.display()))?
        .permissions()
        .mode();
    if mode & 0o777 != 0o600 {
        bail!("{} must have mode 0600", key_file.display());
    }
    let repo = required("SAKA_E2E_REPO")?;
    if repo.split('/').count() != 2 {
        bail!("SAKA_E2E_REPO must be <owner>/<name>");
    }
    let claude_dir = PathBuf::from(required("SAKA_E2E_CLAUDE_CONFIG_DIR")?);
    if !claude_dir.is_dir() {
        bail!(
            "SAKA_E2E_CLAUDE_CONFIG_DIR {} is not a directory",
            claude_dir.display()
        );
    }
    let model = std::env::var("SAKA_E2E_MODEL")
        .ok()
        .filter(|m| !m.is_empty())
        .unwrap_or_else(|| DEFAULT_MODEL.to_string());
    Ok(Settings {
        app_id,
        key_file,
        repo,
        claude_dir,
        model,
    })
}

fn run_id() -> String {
    let now = jiff::Timestamp::now().strftime("%Y%m%d-%H%M%S");
    format!("{now}-{:x}", std::process::id())
}

fn done_when_commands(spec: &str) -> BTreeSet<String> {
    spec.lines()
        .filter(|line| line.starts_with("| S"))
        .filter_map(|line| line.trim_end_matches('|').rsplit('|').next())
        .flat_map(|cell| {
            cell.split('`')
                .skip(1)
                .step_by(2)
                .map(String::from)
                .collect::<Vec<_>>()
        })
        .collect()
}

fn toml_string(value: &str) -> String {
    serde_json::Value::String(value.to_string()).to_string()
}

fn config(settings: &Settings, listen: &str, data_dir: &Path) -> String {
    format!(
        r#"[server]
listen = "{listen}"
data_dir = {data_dir}

[scheduler]
max_active_tasks = 1

[autonomy]
default = "standard"

[[autonomy.repo]]
repo = {repo}
level = "autonomous"

[[scm]]
id = "github"
provider = "github"
app_id = {app_id}
private_key_file = {key_file}
poll_interval_secs = 5
merge_method = "squash"

[agent]
account_dir = {claude_dir}
model = {model}

[budget.dev]
running_minutes = 20

[[permission.rule]]
id = "read-tools"
effect = "allow"
tools = ["Read", "Glob", "Grep", "LS", "TodoWrite"]

[[permission.rule]]
id = "edit-in-worktree"
effect = "allow"
tools = ["Write", "Edit", "MultiEdit", "NotebookEdit"]
paths = ["**"]

[[permission.rule]]
id = "safe-commands"
effect = "allow"
tools = ["Bash"]
commands = ["go -C cli *", "go version", "which go", "go test *", "go vet *", "go build *", "gofmt *", "test -f *", "mkdir *", "git status*", "git diff*", "git add *", "git commit *", "git log*", "ls *", "cat *"]
"#,
        data_dir = toml_string(&data_dir.to_string_lossy()),
        repo = toml_string(&settings.repo),
        app_id = settings.app_id,
        key_file = toml_string(&settings.key_file.to_string_lossy()),
        claude_dir = toml_string(&settings.claude_dir.to_string_lossy()),
        model = toml_string(&settings.model),
    )
}

fn free_loopback_port() -> Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0").context("find a free port")?;
    Ok(listener.local_addr()?.port())
}

struct Server {
    child: Child,
}

impl Server {
    fn start(saka: &Path, config: &Path, log: &Path) -> Result<Server> {
        let out = File::create(log).with_context(|| format!("create {}", log.display()))?;
        let child = Command::new(saka)
            .arg("serve")
            .env("SAKA_CONFIG", config)
            .stdout(out.try_clone()?)
            .stderr(out)
            .spawn()
            .context("start saka serve")?;
        Ok(Server { child })
    }

    fn stop(&mut self) {
        if matches!(self.child.try_wait(), Ok(Some(_))) {
            return;
        }
        let _ = Command::new("kill")
            .args(["-TERM", &self.child.id().to_string()])
            .status();
        let started = Instant::now();
        while started.elapsed() < STOP_LIMIT {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop();
    }
}

struct Api {
    base: String,
    http: reqwest::Client,
    runtime: tokio::runtime::Runtime,
}

impl Api {
    fn new(listen: &str) -> Result<Api> {
        Ok(Api {
            base: format!("http://{listen}"),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .context("build the HTTP client")?,
            runtime: tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .context("start the runtime")?,
        })
    }

    fn status(&self, path: &str) -> Option<u16> {
        let request = self.http.get(format!("{}{path}", self.base));
        self.runtime
            .block_on(async move { request.send().await })
            .ok()
            .map(|r| r.status().as_u16())
    }

    fn get_text(&self, path: &str) -> Result<String> {
        let request = self.http.get(format!("{}{path}", self.base));
        self.runtime.block_on(async {
            let response = request
                .send()
                .await
                .with_context(|| format!("GET {path}"))?;
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            if !status.is_success() {
                bail!("GET {path}: {status}");
            }
            Ok(body)
        })
    }

    fn get(&self, path: &str) -> Result<Value> {
        let request = self.http.get(format!("{}{path}", self.base));
        self.runtime.block_on(async {
            let response = request
                .send()
                .await
                .with_context(|| format!("GET {path}"))?;
            let status = response.status();
            let body = response.json::<Value>().await.unwrap_or(Value::Null);
            if !status.is_success() {
                bail!("GET {path}: {status} {body}");
            }
            Ok(body)
        })
    }
}

struct Cli<'a> {
    saka: &'a Path,
    config: &'a Path,
    touches: RefCell<Vec<String>>,
    prompts: RefCell<Vec<String>>,
}

impl Cli<'_> {
    fn touch(&self, label: String, args: &[&str]) -> Result<Value> {
        let reply = self.run(args)?;
        self.touches.borrow_mut().push(label);
        Ok(reply)
    }

    fn prompt(&self, label: String, args: &[&str]) -> Result<Value> {
        let reply = self.run(args)?;
        self.prompts.borrow_mut().push(label);
        Ok(reply)
    }

    fn run(&self, args: &[&str]) -> Result<Value> {
        let out = Command::new(self.saka)
            .arg("--json")
            .args(args)
            .env("SAKA_CONFIG", self.config)
            .stderr(Stdio::inherit())
            .output()
            .with_context(|| format!("run saka {}", args.join(" ")))?;
        if !out.status.success() {
            bail!(
                "saka {} exited with {}: {}",
                args.join(" "),
                out.status,
                String::from_utf8_lossy(&out.stdout)
            );
        }
        serde_json::from_slice(&out.stdout)
            .with_context(|| format!("parse the output of saka {}", args.join(" ")))
    }
}

fn text(value: &Value) -> &str {
    value.as_str().unwrap_or_default()
}

fn answer_permissions(
    api: &Api,
    cli: &Cli<'_>,
    project: &str,
    allowed: &BTreeSet<String>,
) -> Result<()> {
    let page = api.get(&format!(
        "/api/v1/permission-requests?state=pending&project_id={project}"
    ))?;
    for request in page["items"].as_array().into_iter().flatten() {
        let id = text(&request["id"]);
        let command = text(&request["input"]["command"]);
        let injected = command == SLOW_COMMAND;
        let answer = |label: String, args: &[&str]| {
            if injected {
                cli.run(args).map(|_| ())
            } else {
                cli.prompt(label, args).map(|_| ())
            }
        };
        if text(&request["tool"]) == "Bash" && (allowed.contains(command) || injected) {
            println!("  allow  {command}");
            answer(
                format!("allow {command}"),
                &["permission", "answer", id, "allow"],
            )?;
        } else {
            let summary = text(&request["summary"]);
            println!("  deny   {} {summary}", text(&request["tool"]));
            answer(
                format!("deny {summary}"),
                &[
                    "permission",
                    "answer",
                    id,
                    "deny",
                    "--message",
                    DENY_MESSAGE,
                ],
            )?;
        }
    }
    Ok(())
}

fn short(sha: &str) -> &str {
    sha.get(..7).unwrap_or(sha)
}

fn progress(task: &Value) -> String {
    format!(
        "{} {} {}/{}",
        text(&task["task_key"]),
        text(&task["state"]),
        text(&task["stage"]),
        text(&task["stage_phase"])
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Scenario {
    SteerRunning,
    Interrupt,
    SteerIdle,
    KillRunner,
    RestartServer,
    Done,
}

fn session_state(task: &Value) -> &str {
    text(&task["session"]["state"])
}

fn exercise(
    stage: &mut Scenario,
    task: &Value,
    cli: &Cli<'_>,
    api: &Api,
    restart: &mut dyn FnMut() -> Result<()>,
) -> Result<()> {
    let id = text(&task["id"]);
    let state = session_state(task);
    let resumed = || -> Result<bool> {
        let sessions = api.get(&format!("/api/v1/sessions?work_item_id={id}"))?;
        Ok(sessions["items"]
            .as_array()
            .is_some_and(|s| s.iter().any(|x| text(&x["resume_cause"]) == "lost")))
    };
    let next = match (*stage, state) {
        (Scenario::SteerRunning, "running") => {
            println!("  steer   {} while its turn runs", text(&task["task_key"]));
            cli.run(&["task", "steer", id, "Keep every file you write short."])?;
            Scenario::Interrupt
        }
        (Scenario::Interrupt, "running") => {
            println!("  interrupt {}", text(&task["task_key"]));
            cli.run(&["task", "interrupt", id])?;
            Scenario::SteerIdle
        }
        (Scenario::Interrupt, "idle") => Scenario::SteerIdle,
        (Scenario::SteerIdle, "idle") => {
            println!("  steer   {} from idle", text(&task["task_key"]));
            cli.run(&[
                "task",
                "steer",
                id,
                "First run `sleep 30` once, then continue the step from where you stopped and commit.",
            ])?;
            Scenario::KillRunner
        }
        (Scenario::KillRunner, "running" | "waiting") => {
            let session = api.get(&format!("/api/v1/sessions?work_item_id={id}"))?;
            let unit = session["items"]
                .as_array()
                .and_then(|s| s.last())
                .map(|s| format!("saka-session-{}", text(&s["id"])))
                .unwrap_or_default();
            println!("  kill    {unit}");
            Command::new("systemctl")
                .args(["--user", "kill", "--signal=SIGKILL", &unit])
                .status()
                .context("kill the runner unit")?;
            Scenario::RestartServer
        }
        (Scenario::RestartServer, "running" | "waiting") if resumed()? => {
            println!("  restart saka serve while the resumed session runs");
            restart()?;
            Scenario::Done
        }
        (current, _) => current,
    };
    *stage = next;
    Ok(())
}

fn check_runtime(api: &Api, task: &str) -> Result<()> {
    let sessions = api.get(&format!("/api/v1/sessions?work_item_id={task}"))?;
    let items = sessions["items"].as_array().cloned().unwrap_or_default();
    let linked = items.windows(2).any(|w| {
        text(&w[1]["resumed_from_session_id"]) == text(&w[0]["id"])
            && text(&w[1]["resume_cause"]) == "lost"
    });
    if !linked {
        bail!("S1 has no session resumed after the runner was killed: {sessions}");
    }
    let mut kinds = BTreeSet::new();
    for s in &items {
        let timeline = api.get(&format!(
            "/api/v1/sessions/{}/timeline?limit=500",
            text(&s["id"])
        ))?;
        for row in timeline["items"].as_array().into_iter().flatten() {
            kinds.insert(text(&row["kind"]).to_string());
        }
    }
    for kind in ["steer", "result"] {
        if !kinds.contains(kind) {
            bail!("the S1 timeline has no {kind} activity: {kinds:?}");
        }
    }
    println!(
        "  S1 ran {} sessions; timeline kinds {kinds:?}",
        items.len()
    );
    Ok(())
}

struct ContextDoc {
    path: String,
    marker: String,
}

fn check_packet(api: &Api, task: &str, spec: &str, context: &ContextDoc) -> Result<()> {
    let packet = api.get(&format!("/api/v1/work-items/{task}/work-packet"))?;
    let spec = api.get(&format!("/api/v1/specs/{spec}"))?;
    let pinned = spec["revision"]["context"]
        .as_array()
        .and_then(|c| c.first())
        .cloned()
        .unwrap_or(Value::Null);
    let doc = packet["documents"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|d| d["path"] == context.path.as_str())
        .ok_or_else(|| anyhow!("the S1 work packet has no {}", context.path))?;
    if doc["origin"] != "link" || doc["content_hash"] != pinned["content_hash"] {
        bail!(
            "the S1 work packet does not carry the pinned {}: {doc}",
            context.path
        );
    }
    let source = format!("<source>{} @ ", context.path);
    let content = text(&packet["text"])
        .split_once(&source)
        .and_then(|(_, rest)| rest.split_once("</document_content>"))
        .map(|(content, _)| content)
        .unwrap_or_default();
    if !content.contains(&context.marker) {
        bail!("the S1 work packet has {} without its marker", context.path);
    }
    println!(
        "  S1 work packet carries {} ({} bytes)",
        context.path, packet["bytes"]
    );
    Ok(())
}

struct Target<'a> {
    project: &'a str,
    spec: &'a str,
    run_id: &'a str,
    allowed: &'a BTreeSet<String>,
    context: &'a ContextDoc,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Rework {
    Waiting,
    Interrupted,
    Steered,
}

fn rework_steer(run_id: &str) -> String {
    format!(
        "Change of plan from the operator, for this first commit only: write `e2e-runs/{run_id}/s2.md` as a single line, `placeholder`, and do not quote anything from `e2e-runs/{run_id}/s1.md` yet. Commit it and stop. A later review round will ask for the real content; do what that review asks then."
    )
}

fn rework_step(phase: &mut Rework, task: &Value, cli: &Cli<'_>, run_id: &str) -> Result<()> {
    let (id, key) = (text(&task["id"]), text(&task["task_key"]));
    match (*phase, session_state(task)) {
        (Rework::Waiting, "running") => {
            println!("  interrupt {key}, to leave its requirement unmet");
            cli.run(&["task", "interrupt", id])?;
            *phase = Rework::Interrupted;
        }
        (Rework::Interrupted, "idle") => {
            println!("  steer   {key} to write a placeholder");
            cli.run(&["task", "steer", id, &rework_steer(run_id)])?;
            *phase = Rework::Steered;
        }
        _ => {}
    }
    Ok(())
}

fn deliver(
    api: &Api,
    cli: &Cli<'_>,
    target: &Target<'_>,
    restart: &mut dyn FnMut() -> Result<()>,
) -> Result<()> {
    let spec = target.spec;
    let mut packet_checked = false;
    let started = Instant::now();
    let mut seen: BTreeMap<String, String> = BTreeMap::new();
    let mut stage = Scenario::SteerRunning;
    let mut rework = Rework::Waiting;
    let mut first: Option<String> = None;
    loop {
        if started.elapsed() > RUN_LIMIT {
            bail!(
                "the spec did not ship within {} minutes",
                RUN_LIMIT.as_secs() / 60
            );
        }
        answer_permissions(api, cli, target.project, target.allowed)?;
        let tasks = api.get(&format!("/api/v1/work-items?spec_id={spec}&limit=200"))?;
        for task in tasks["items"].as_array().into_iter().flatten() {
            let id = text(&task["id"]).to_string();
            let line = progress(task);
            if seen.get(&id) != Some(&line) {
                println!("  {line}");
                seen.insert(id.clone(), line);
            }
            if task["merge_auto"] != true {
                bail!(
                    "{} would wait for a merge tap: merge_auto is {} at autonomy {}",
                    text(&task["task_key"]),
                    task["merge_auto"],
                    task["autonomy"]
                );
            }
            if task["step_id"] == "S1" {
                first.get_or_insert_with(|| id.clone());
                if !packet_checked && text(&task["stage_phase"]) == "running_session" {
                    check_packet(api, &id, spec, target.context)?;
                    packet_checked = true;
                }
                if stage != Scenario::Done && text(&task["stage_phase"]) == "running_session" {
                    exercise(&mut stage, task, cli, api, restart)?;
                }
            }
            if task["step_id"] == "S2" {
                let writing =
                    task["stage"] == "dev" && text(&task["stage_phase"]) == "running_session";
                if writing {
                    rework_step(&mut rework, task, cli, target.run_id)?;
                } else if rework != Rework::Steered
                    && !task["stage"].is_null()
                    && task["stage"] != "dev"
                {
                    bail!(
                        "S2 left its dev stage ({}) before the scenario could leave its requirement unmet",
                        progress(task)
                    );
                }
            }
            if text(&task["state"]) == "blocked" {
                bail!(
                    "{} is blocked: {}",
                    text(&task["task_key"]),
                    task["blocker"]
                );
            }
        }
        if text(&api.get(&format!("/api/v1/specs/{spec}"))?["state"]) == "shipped" {
            if stage != Scenario::Done {
                bail!("S1 finished before the runtime scenario did ({stage:?})");
            }
            if !packet_checked {
                bail!("S1 shipped before its work packet was checked");
            }
            check_advance(api, spec)?;
            return check_runtime(api, first.as_deref().unwrap_or_default());
        }
        std::thread::sleep(POLL);
    }
}

fn timestamp(value: &Value, what: &str) -> Result<jiff::Timestamp> {
    value
        .as_str()
        .with_context(|| format!("{what} is missing"))?
        .parse()
        .with_context(|| format!("{what} is not a timestamp"))
}

fn check_advance(api: &Api, spec: &str) -> Result<()> {
    let tasks = api.get(&format!("/api/v1/work-items?spec_id={spec}&limit=200"))?;
    let step = |id: &str| {
        tasks["items"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|t| t["step_id"] == id)
            .with_context(|| format!("{id} is missing"))
    };
    let merged = timestamp(&step("S1")?["change_request"]["merged_at"], "S1 merged_at")?;
    let admitted = timestamp(&step("S2")?["admitted_at"], "S2 admitted_at")?;
    let waited = Duration::try_from(admitted.duration_since(merged)).unwrap_or_default();
    println!("  S2 admitted {}s after S1 merged", waited.as_secs());
    if waited > ADVANCE_LIMIT {
        bail!(
            "S2 was admitted {}s after S1 merged (limit {}s)",
            waited.as_secs(),
            ADVANCE_LIMIT.as_secs()
        );
    }
    Ok(())
}

fn verify(github: &GitHub, base: &str) -> Result<()> {
    let pulls = github.pull_requests(base)?;
    let merged = pulls.iter().filter(|p| p.merged).count();
    if pulls.len() != 2 || merged != 2 {
        bail!(
            "expected two merged pull requests into {base}, found {} ({merged} merged)",
            pulls.len()
        );
    }
    let started = Instant::now();
    for pull in &pulls {
        while github.branch_exists(&pull.head)? {
            if started.elapsed() > CLEANUP_LIMIT {
                bail!(
                    "task branch {} (#{}) was not deleted",
                    pull.head,
                    pull.number
                );
            }
            std::thread::sleep(POLL);
        }
        println!("  #{} merged, {} deleted", pull.number, pull.head);
    }
    Ok(())
}

fn wait_ready(api: &Api) -> Result<()> {
    let started = Instant::now();
    while api.status("/readyz") != Some(200) {
        if started.elapsed() > READY_LIMIT {
            bail!("saka serve did not become ready");
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    Ok(())
}

struct Run<'a> {
    settings: &'a Settings,
    run_id: String,
    saka: PathBuf,
    dir: PathBuf,
    base: String,
    spec_path: String,
    spec: String,
    context: ContextDoc,
}

fn drive(run: &Run<'_>, github: &GitHub) -> Result<()> {
    let listen = format!("127.0.0.1:{}", free_loopback_port()?);
    let config_path = run.dir.join("config.toml");
    std::fs::write(
        &config_path,
        config(run.settings, &listen, &run.dir.join("data")),
    )
    .context("write the config")?;
    let log = run.dir.join("serve.log");
    let mut server = Some(Server::start(&run.saka, &config_path, &log)?);
    let api = Api::new(&listen)?;
    wait_ready(&api)?;
    let mut restart = || -> Result<()> {
        if let Some(mut old) = server.take() {
            old.stop();
        }
        server = Some(Server::start(
            &run.saka,
            &config_path,
            &run.dir.join("serve-2.log"),
        )?);
        wait_ready(&api)
    };
    let cli = Cli {
        saka: &run.saka,
        config: &config_path,
        touches: RefCell::new(Vec::new()),
        prompts: RefCell::new(Vec::new()),
    };
    let url = format!("https://github.com/{}", run.settings.repo);
    let project = cli.run(&[
        "project",
        "add",
        &url,
        "--path",
        "docs",
        "--base-branch",
        &run.base,
    ])?;
    let project_id = text(&project["id"]).to_string();
    let spec = cli.touch(
        "approve the spec".to_string(),
        &[
            "spec",
            "approve",
            &project_id,
            &run.spec_path,
            "--reason",
            "end-to-end run",
        ],
    )?;
    let spec_id = text(&spec["id"]).to_string();
    println!(
        "  approved {} ({} steps)",
        run.spec_path,
        spec["steps"].as_array().map_or(0, Vec::len)
    );
    deliver(
        &api,
        &cli,
        &Target {
            project: &project_id,
            spec: &spec_id,
            run_id: &run.run_id,
            allowed: &done_when_commands(&run.spec),
            context: &run.context,
        },
        &mut restart,
    )?;
    println!("  spec shipped");
    verify(github, &run.base)?;
    gate::check(&api, &cli, github, &spec_id)?;
    outside::review_only(&api, &cli, run)?;
    draft(&api, &cli, github, &project_id, run)?;
    conversation(&api, &cli, github, &project_id, run)
}

fn draft(api: &Api, cli: &Cli<'_>, github: &GitHub, project: &str, run: &Run<'_>) -> Result<()> {
    let path = format!("docs/specs/e2e-draft-{}.md", run.run_id);
    println!("  architect drafts {path}");
    let draft = cli.run(&[
        "spec", "draft", project, &path, "--idea", DRAFT_IDEA, "--wait",
    ])?;
    if text(&draft["state"]) != "succeeded" {
        bail!("the draft did not succeed: {draft}");
    }
    let commit = text(&draft["commit"]);
    let spec = api.get(&format!("/api/v1/specs/{}", text(&draft["spec_id"])))?;
    if spec["origin"] != "architect" || spec["path"] != path.as_str() {
        bail!("the drafted spec is not registered from the architect: {spec}");
    }
    let tip = github.last_commit_touching(&run.base, &path)?;
    if tip.as_deref() != Some(commit) {
        bail!(
            "{path} on {} was last changed by {tip:?}, not by the draft commit {commit}",
            run.base
        );
    }
    let tools = architect_tools(api, text(&draft["id"]))?;
    println!(
        "  draft succeeded in {} round(s), commit {commit}, tools {tools:?}",
        draft["last_round"]["round"]
    );
    Ok(())
}

fn architect_tools(api: &Api, draft: &str) -> Result<BTreeSet<String>> {
    let sessions = api.get(&format!("/api/v1/sessions?spec_draft_id={draft}"))?;
    let mut tools = BTreeSet::new();
    for s in sessions["items"].as_array().into_iter().flatten() {
        let mut after = String::new();
        loop {
            let page = api.get(&format!(
                "/api/v1/sessions/{}/timeline?limit=500{after}",
                text(&s["id"])
            ))?;
            for row in page["items"].as_array().into_iter().flatten() {
                if row["kind"] == "action" {
                    tools.insert(text(&row["data"]["tool"]).to_string());
                }
            }
            match page["next_after"].as_u64() {
                Some(next) => after = format!("&after={next}"),
                None => break,
            }
        }
    }
    let outside: Vec<&String> = tools
        .iter()
        .filter(|t| !ARCHITECT_TOOLS.contains(&t.as_str()))
        .collect();
    if !outside.is_empty() {
        bail!("the architect session used tools outside its allow-list: {outside:?}");
    }
    Ok(tools)
}

fn wait_for_draft(api: &Api, draft: &str, state: &str, rounds: u64) -> Result<Value> {
    let started = Instant::now();
    loop {
        let current = api.get(&format!("/api/v1/spec-drafts/{draft}"))?;
        let now = text(&current["state"]);
        if now == state && current["attempt"]["rounds"].as_u64() >= Some(rounds) {
            return Ok(current);
        }
        if matches!(now, "failed" | "cancelled") {
            bail!("the conversation ended as {now}: {current}");
        }
        if started.elapsed() > CONVERSATION_LIMIT {
            bail!("the conversation did not reach {state} (round {rounds}): {current}");
        }
        std::thread::sleep(POLL);
    }
}

fn lint_reply(changes: &Value, path: &str) -> Option<String> {
    if changes["lint"]["passed"] == true {
        return None;
    }
    let findings = &changes["lint"]["findings"];
    Some(format!(
        "The lint of {path} reports: {findings}. Fix every finding except decision_unanswered without asking me anything, change nothing else, then end your turn."
    ))
}

fn changes_past_the_lint(api: &Api, cli: &Cli<'_>, id: &str, path: &str) -> Result<Value> {
    let url = format!("/api/v1/spec-drafts/{id}/changes");
    let mut changes = api.get(&url)?;
    for round in FIRST_LINT_REPLY_ROUND..FIRST_LINT_REPLY_ROUND + LINT_REPLIES {
        let Some(reply) = lint_reply(&changes, path) else {
            return Ok(changes);
        };
        cli.run(&["draft", "message", id, &reply])?;
        wait_for_draft(api, id, "awaiting_operator", round)?;
        changes = api.get(&url)?;
    }
    if lint_reply(&changes, path).is_some() {
        bail!("the conversation's spec does not pass the lint: {changes}");
    }
    Ok(changes)
}

fn conversation(
    api: &Api,
    cli: &Cli<'_>,
    github: &GitHub,
    project: &str,
    run: &Run<'_>,
) -> Result<()> {
    let path = format!("docs/specs/e2e-session-{}.md", run.run_id);
    let notes = format!("docs/e2e-notes-{}.md", run.run_id);
    println!("  architect converses about {path}");
    let started = cli.run(&[
        "spec", "session", project, &path, "--idea", DRAFT_IDEA, "--wait",
    ])?;
    let id = text(&started["id"]).to_string();
    if text(&started["state"]) != "awaiting_operator" {
        bail!("the conversation did not wait for the operator: {started}");
    }
    let reply = format!(
        "Thanks. Also write a short note on how the counts are meant to be read to {notes}: a markdown file of a few lines. Keep the spec as it is, then end your turn."
    );
    cli.run(&["draft", "message", &id, &reply])?;
    wait_for_draft(api, &id, "awaiting_operator", 2)?;
    let changes = changes_past_the_lint(api, cli, &id, &path)?;
    let changed: Vec<&str> = changes["files"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|f| text(&f["path"]))
        .collect();
    if !changed.contains(&path.as_str()) || !changed.contains(&notes.as_str()) {
        bail!("the conversation did not change both files: {changes}");
    }
    let committed = cli.run(&[
        "draft",
        "commit",
        &id,
        "--message",
        "docs: add the counts spec and its note",
    ])?;
    if text(&committed["state"]) != "committing" {
        bail!("the commit did not start: {committed}");
    }
    let done = wait_for_draft(api, &id, "succeeded", 2)?;
    let commit = text(&done["commit"]);
    for file in [&path, &notes] {
        let tip = github.last_commit_touching(&run.base, file)?;
        if tip.as_deref() != Some(commit) {
            bail!(
                "{file} on {} was last changed by {tip:?}, not by the conversation commit {commit}",
                run.base
            );
        }
    }
    let spec = api.get(&format!("/api/v1/specs/{}", text(&done["spec_id"])))?;
    if spec["origin"] != "architect" || spec["path"] != path.as_str() {
        bail!("the conversation's spec is not registered from the architect: {spec}");
    }
    let tools = architect_tools(api, &id)?;
    println!("  conversation committed {commit} with both files, tools {tools:?}");
    Ok(())
}

fn build_saka(root: &Path) -> Result<PathBuf> {
    let status = Command::new(metadata::cargo())
        .args(["build", "--locked", "-p", "saka"])
        .current_dir(root)
        .status()
        .context("build saka")?;
    if !status.success() {
        bail!("cargo build -p saka exited with {status}");
    }
    let target = metadata::target_directory(&metadata::load()?)?;
    Ok(target.join("debug/saka"))
}

pub fn run() -> Result<()> {
    let settings = settings()?;
    let root = metadata::workspace_root()?;
    let saka = build_saka(&root)?;
    let run_id = run_id();
    let dir = root.join("target/e2e").join(&run_id);
    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    let base = format!("e2e/{run_id}");
    let spec_path = format!("docs/specs/e2e-{run_id}.md");
    let spec = TEMPLATE.replace("{run_id}", &run_id);
    let context = ContextDoc {
        path: format!("docs/e2e-context-{run_id}.md"),
        marker: format!("context-marker: {run_id}"),
    };
    println!(
        "==> end-to-end run {run_id} on {} (model {})",
        settings.repo, settings.model
    );

    let github = GitHub::connect(settings.app_id, &settings.key_file, &settings.repo)?;
    github.create_branch(&base, &github.default_branch_sha()?)?;
    github.put_file(
        &base,
        &context.path,
        &CONTEXT_TEMPLATE.replace("{run_id}", &run_id),
        "docs: add end-to-end context",
    )?;
    github.put_file(&base, &spec_path, &spec, "docs: add an end-to-end spec")?;
    let review_config = outside::saka_toml(&settings.repo, &run_id);
    github.put_file(
        &base,
        &format!("{}/saka.toml", outside::PROJECT_PATH),
        &review_config,
        "docs: add a review-only project",
    )?;
    println!("  base branch {base} with {spec_path}");

    let run = Run {
        settings: &settings,
        run_id: run_id.clone(),
        saka,
        dir,
        base,
        spec_path,
        spec,
        context,
    };
    match drive(&run, &github) {
        Ok(()) => {
            github.delete_branch(&run.base)?;
            println!("  base branch {} deleted", run.base);
            println!("e2e: ok");
            Ok(())
        }
        Err(e) => {
            eprintln!(
                "kept base branch {} and the server log at {}",
                run.base,
                run.dir.join("serve.log").display()
            );
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_spec_past_the_lint_needs_no_reply() {
        let changes = serde_json::json!({ "lint": { "passed": true, "findings": [] } });
        assert_eq!(lint_reply(&changes, "docs/specs/a.md"), None);
    }

    #[test]
    fn a_refused_spec_gets_its_findings_back_as_a_reply() {
        let changes = serde_json::json!({
            "lint": { "passed": false, "findings": [{ "code": "step_ac_unknown", "line": 22 }] }
        });
        let reply = lint_reply(&changes, "docs/specs/a.md").unwrap();
        assert!(reply.contains("docs/specs/a.md"));
        assert!(reply.contains("step_ac_unknown"));
    }

    #[test]
    fn the_fixture_spec_names_its_done_when_commands() {
        let spec = TEMPLATE.replace("{run_id}", "r1");
        assert!(!spec.contains("{run_id}"));
        assert!(spec.contains("](../e2e-context-r1.md)"));
        assert!(!spec.contains("context-marker"));
        assert!(spec.contains("| S2 |"));
        assert_eq!(
            done_when_commands(&spec),
            BTreeSet::from([
                "go -C cli test ./...".to_string(),
                "test -f e2e-runs/r1/s1.md".to_string(),
                "test -f e2e-runs/r1/s2.md".to_string(),
            ])
        );
    }

    #[test]
    fn the_generated_config_quotes_paths() {
        let settings = Settings {
            app_id: 7,
            key_file: PathBuf::from("/k \"x\".pem"),
            repo: "acme/app".into(),
            claude_dir: PathBuf::from("/c"),
            model: "haiku".into(),
        };
        let text = config(&settings, "127.0.0.1:1", Path::new("/d"));
        assert!(text.contains("private_key_file = \"/k \\\"x\\\".pem\""));
        assert!(text.contains("app_id = 7"));
    }

    #[test]
    fn the_generated_config_lifts_the_repo_to_autonomous() {
        let settings = Settings {
            app_id: 7,
            key_file: PathBuf::from("/k.pem"),
            repo: "acme/app".into(),
            claude_dir: PathBuf::from("/c"),
            model: "haiku".into(),
        };
        let text = config(&settings, "127.0.0.1:1", Path::new("/d"));
        let parsed: toml::Table = text.parse().unwrap_or_default();
        assert_eq!(parsed["autonomy"]["default"].as_str(), Some("standard"));
        let repos = parsed["autonomy"]["repo"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert_eq!(repos.len(), 1);
        assert_eq!(repos[0]["repo"].as_str(), Some("acme/app"));
        assert_eq!(repos[0]["level"].as_str(), Some("autonomous"));
        assert!(text.contains("\"go -C cli *\""));
    }

    #[test]
    fn the_rework_steer_names_the_file_and_defers_to_the_review() {
        let steer = rework_steer("r1");
        assert!(steer.contains("e2e-runs/r1/s2.md"));
        assert!(steer.contains("e2e-runs/r1/s1.md"));
        assert!(steer.contains("later review round"));
    }

    #[test]
    fn decisions_are_touches_prompts_are_listed_and_injections_are_not() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("tempdir: {e}"));
        let fake = dir.path().join("saka");
        std::fs::write(&fake, "#!/bin/sh\necho '{}'\n").unwrap_or_else(|e| panic!("write: {e}"));
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755))
            .unwrap_or_else(|e| panic!("chmod: {e}"));
        let cli = Cli {
            saka: &fake,
            config: Path::new("/c"),
            touches: RefCell::new(Vec::new()),
            prompts: RefCell::new(Vec::new()),
        };
        assert!(cli.run(&["task", "steer"]).is_ok());
        assert!(cli.touches.borrow().is_empty());
        assert!(cli.touch("approve".into(), &["spec", "approve"]).is_ok());
        assert_eq!(*cli.touches.borrow(), vec!["approve".to_string()]);
        assert!(
            cli.prompt("allow ls".into(), &["permission", "answer"])
                .is_ok()
        );
        assert_eq!(cli.touches.borrow().len(), 1);
        assert_eq!(cli.prompts.borrow().len(), 1);
    }
}
