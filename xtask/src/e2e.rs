mod github;

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::metadata;
use github::GitHub;

const TEMPLATE: &str = include_str!("e2e/fixture-spec.md.tmpl");
const RUN_LIMIT: Duration = Duration::from_secs(30 * 60);
const READY_LIMIT: Duration = Duration::from_secs(60);
const CLEANUP_LIMIT: Duration = Duration::from_secs(3 * 60);
const STOP_LIMIT: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_secs(5);
const DEFAULT_MODEL: &str = "haiku";
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
        .with_context(|| format!("{name} is not set (see the end-to-end section of the README)"))
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
commands = ["go test *", "go vet *", "go build *", "gofmt *", "test -f *", "mkdir *", "git status*", "git diff*", "git add *", "git commit *", "git log*", "ls *", "cat *"]
"#,
        data_dir = toml_string(&data_dir.to_string_lossy()),
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
}

impl Cli<'_> {
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
        if text(&request["tool"]) == "Bash" && allowed.contains(command) {
            println!("  allow  {command}");
            cli.run(&["permission", "answer", id, "allow"])?;
        } else {
            println!(
                "  deny   {} {}",
                text(&request["tool"]),
                text(&request["summary"])
            );
            cli.run(&[
                "permission",
                "answer",
                id,
                "deny",
                "--message",
                DENY_MESSAGE,
            ])?;
        }
    }
    Ok(())
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

fn deliver(
    api: &Api,
    cli: &Cli<'_>,
    project: &str,
    spec: &str,
    allowed: &BTreeSet<String>,
) -> Result<()> {
    let started = Instant::now();
    let mut merged = BTreeSet::new();
    let mut seen: BTreeMap<String, String> = BTreeMap::new();
    loop {
        if started.elapsed() > RUN_LIMIT {
            bail!(
                "the spec did not ship within {} minutes",
                RUN_LIMIT.as_secs() / 60
            );
        }
        answer_permissions(api, cli, project, allowed)?;
        let tasks = api.get(&format!("/api/v1/work-items?spec_id={spec}&limit=200"))?;
        for task in tasks["items"].as_array().into_iter().flatten() {
            let id = text(&task["id"]).to_string();
            let line = progress(task);
            if seen.get(&id) != Some(&line) {
                println!("  {line}");
                seen.insert(id.clone(), line);
            }
            if text(&task["state"]) == "blocked" {
                bail!(
                    "{} is blocked: {}",
                    text(&task["task_key"]),
                    task["blocker"]
                );
            }
            let can_merge = task["available_actions"]
                .as_array()
                .is_some_and(|a| a.iter().any(|x| x == "merge"));
            if can_merge && merged.insert(id.clone()) {
                println!("  merge  {}", text(&task["task_key"]));
                cli.run(&["task", "merge", &id, "--reason", "end-to-end run"])?;
            }
        }
        if text(&api.get(&format!("/api/v1/specs/{spec}"))?["state"]) == "shipped" {
            return Ok(());
        }
        std::thread::sleep(POLL);
    }
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
    saka: PathBuf,
    dir: PathBuf,
    base: String,
    spec_path: String,
    spec: String,
}

fn drive(run: &Run<'_>, github: &GitHub) -> Result<()> {
    let listen = format!("127.0.0.1:{}", free_loopback_port()?);
    let config_path = run.dir.join("config.toml");
    std::fs::write(
        &config_path,
        config(run.settings, &listen, &run.dir.join("data")),
    )
    .context("write the config")?;
    let _server = Server::start(&run.saka, &config_path, &run.dir.join("serve.log"))?;
    let api = Api::new(&listen)?;
    wait_ready(&api)?;
    let cli = Cli {
        saka: &run.saka,
        config: &config_path,
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
    let spec = cli.run(&[
        "spec",
        "approve",
        &project_id,
        &run.spec_path,
        "--reason",
        "end-to-end run",
    ])?;
    let spec_id = text(&spec["id"]).to_string();
    println!(
        "  approved {} ({} steps)",
        run.spec_path,
        spec["steps"].as_array().map_or(0, Vec::len)
    );
    deliver(
        &api,
        &cli,
        &project_id,
        &spec_id,
        &done_when_commands(&run.spec),
    )?;
    println!("  spec shipped");
    verify(github, &run.base)
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
    Ok(root.join("target/debug/saka"))
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
    println!(
        "==> end-to-end run {run_id} on {} (model {})",
        settings.repo, settings.model
    );

    let github = GitHub::connect(settings.app_id, &settings.key_file, &settings.repo)?;
    github.create_branch(&base, &github.default_branch_sha()?)?;
    github.put_file(&base, &spec_path, &spec, "docs: add an end-to-end spec")?;
    println!("  base branch {base} with {spec_path}");

    let run = Run {
        settings: &settings,
        saka,
        dir,
        base,
        spec_path,
        spec,
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
    fn the_fixture_spec_names_its_done_when_commands() {
        let spec = TEMPLATE.replace("{run_id}", "r1");
        assert!(!spec.contains("{run_id}"));
        assert_eq!(
            done_when_commands(&spec),
            BTreeSet::from([
                "cd cli && go test ./...".to_string(),
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
}
