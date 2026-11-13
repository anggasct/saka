use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use super::github::GitHub;
use super::issue::{Wait, wait_within};
use super::{
    Api, Cli, POLL, Run, answer_permissions, progress, short, text, timestamp, toml_string,
};

const SAKA_TOML: &str = "docs/saka.toml";
pub const ENVIRONMENT: &str = "e2e";
const DEPLOY_COMMAND: &str = "printf %s \"$SAKA_SHA\" > sha";
const READY: &str = "curl -fsS \"$SAKA_DEPLOY_URL/version\" | grep -qx \"$SAKA_SHA\"";
const HEALTH: &str = "curl -fsS \"$SAKA_DEPLOY_URL/health\"";
const NO_BREAK: &str = "test ! -e cli/E2E_BREAK";
const BREAK_DONE_WHEN: &str = "test -f cli/E2E_BREAK";
const BREAK_DESCRIPTION: &str = "Create the file `cli/E2E_BREAK` with the Write tool. It holds one line of text. Change nothing else.";
const BREAK_AC: &str = "`cli/E2E_BREAK` exists and holds one line.";
const REVERT_REASON: &str = "the break marker fails the deploy verification";
const VERIFIED_CHECKS: usize = 2;
const ACCEPT_LIMIT: Duration = Duration::from_secs(2);
const SERVE_PAUSE: Duration = Duration::from_millis(25);
const TASK_LIMIT: Duration = Duration::from_secs(25 * 60);
const REVERT_LIMIT: Duration = Duration::from_secs(20 * 60);

pub struct Target {
    port: u16,
    dir: PathBuf,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Target {
    pub fn start(dir: &Path) -> Result<Target> {
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
        let listener = TcpListener::bind("127.0.0.1:0").context("bind the deploy target")?;
        listener
            .set_nonblocking(true)
            .context("make the deploy target non-blocking")?;
        let port = listener.local_addr()?.port();
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let (stop, dir) = (Arc::clone(&stop), dir.to_path_buf());
            std::thread::spawn(move || serve(&listener, &dir, &stop))
        };
        Ok(Target {
            port,
            dir: dir.to_path_buf(),
            stop,
            thread: Some(thread),
        })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn get(&self, path: &str) -> Result<(u16, String)> {
        let mut stream = TcpStream::connect(("127.0.0.1", self.port))
            .with_context(|| format!("connect to the deploy target for {path}"))?;
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        write!(stream, "GET {path} HTTP/1.0\r\nHost: localhost\r\n\r\n")?;
        let mut raw = String::new();
        stream.read_to_string(&mut raw)?;
        let (head, body) = raw
            .split_once("\r\n\r\n")
            .context("the deploy target sent a malformed reply")?;
        let status = head
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .context("the deploy target sent no status")?;
        Ok((status, body.to_string()))
    }

    pub fn version(&self) -> Result<String> {
        match self.get("/version")? {
            (200, body) => Ok(body),
            (status, _) => bail!("GET /version on the deploy target answered {status}"),
        }
    }
}

impl Drop for Target {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn serve(listener: &TcpListener, dir: &Path, stop: &AtomicBool) {
    while !stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _)) => {
                let _ = answer(stream, dir);
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(SERVE_PAUSE);
            }
            Err(_) => break,
        }
    }
}

fn answer(mut stream: TcpStream, dir: &Path) -> std::io::Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(ACCEPT_LIMIT))?;
    let mut buffer = [0u8; 2048];
    let read = stream.read(&mut buffer)?;
    let request = String::from_utf8_lossy(buffer.get(..read).unwrap_or_default());
    let mut line = request
        .lines()
        .next()
        .unwrap_or_default()
        .split_whitespace();
    let (status, reason, body) = route(line.next(), line.next(), dir);
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn route(method: Option<&str>, path: Option<&str>, dir: &Path) -> (u16, &'static str, String) {
    match (method, path) {
        (Some("GET"), Some("/health")) => (200, "OK", "ok".to_string()),
        (Some("GET"), Some("/version")) => match std::fs::read_to_string(dir.join("sha")) {
            Ok(sha) => (200, "OK", sha),
            Err(_) => (404, "Not Found", "no version yet".to_string()),
        },
        _ => (404, "Not Found", "not found".to_string()),
    }
}

pub fn environment_block(project: &str, target: &Target) -> String {
    format!(
        r#"
[[deploy]]
project = {project}
environment = {environment}
action = "run_host"
command = {command}
cwd = {cwd}
target_url = {url}
gate = true
on_failure = "revert"
timeout_secs = 60
"#,
        project = toml_string(project),
        environment = toml_string(ENVIRONMENT),
        command = toml_string(DEPLOY_COMMAND),
        cwd = toml_string(&target.dir().to_string_lossy()),
        url = toml_string(&format!("http://127.0.0.1:{}", target.port())),
    )
}

pub fn project_name(saka_toml: &str) -> Result<String> {
    let table: toml::Table = saka_toml.parse().context("parse docs/saka.toml")?;
    table
        .get("project")
        .and_then(|p| p.get("name"))
        .and_then(toml::Value::as_str)
        .map(String::from)
        .with_context(|| format!("{SAKA_TOML} has no project name"))
}

pub fn with_verify(original: &str) -> Result<String> {
    let table: toml::Table = original.parse().context("parse docs/saka.toml")?;
    if table.contains_key("verify") {
        bail!("{SAKA_TOML} already has a verify table");
    }
    let commands = [HEALTH, NO_BREAK].map(toml_string).join(", ");
    Ok(format!(
        "{}\n\n[verify.{ENVIRONMENT}]\nready = {}\nready_interval_secs = 5\nready_timeout_secs = 120\ncommands = [{commands}]\n",
        original.trim_end(),
        toml_string(READY),
    ))
}

pub fn prepare_base(github: &GitHub, base: &str) -> Result<String> {
    let (original, blob) = github.file(base, SAKA_TOML)?;
    github.update_file(
        base,
        SAKA_TOML,
        &with_verify(&original)?,
        "docs: verify the end-to-end deployment",
        &blob,
    )?;
    project_name(&original)
}

fn phase_checks<'a>(deployment: &'a Value, phase: &str) -> Vec<&'a Value> {
    deployment["checks"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|c| c["phase"] == phase)
        .collect()
}

pub fn healthy_evidence(deployment: &Value) -> Result<String> {
    for phase in ["deploy", "ready", "verify"] {
        let checks = phase_checks(deployment, phase);
        if checks.is_empty() || checks.iter().any(|c| c["state"] != "passed") {
            bail!(
                "the {phase} phase of deployment {} has no passing evidence: {}",
                text(&deployment["id"]),
                deployment["checks"]
            );
        }
    }
    let verified = phase_checks(deployment, "verify").len();
    if verified != VERIFIED_CHECKS {
        bail!(
            "deployment {} ran {verified} verify check(s), expected {VERIFIED_CHECKS}",
            text(&deployment["id"])
        );
    }
    Ok(format!(
        "deploy, ready and {verified} verify checks passed after {} ready attempt(s)",
        phase_checks(deployment, "ready")
            .first()
            .map_or(0, |c| c["attempts"].as_u64().unwrap_or(0))
    ))
}

pub fn broken_evidence(deployment: &Value) -> Result<()> {
    if deployment["state"] != "failed"
        || deployment["phase"] != "verify"
        || deployment["error"]["code"] != "verify_failed"
    {
        bail!(
            "deployment {} did not fail in verify with verify_failed: state {} phase {} error {}",
            text(&deployment["id"]),
            deployment["state"],
            deployment["phase"],
            deployment["error"]
        );
    }
    let state_of = |command: &str| {
        phase_checks(deployment, "verify")
            .into_iter()
            .find(|c| text(&c["command"]) == command)
            .map(|c| text(&c["state"]).to_string())
    };
    if state_of(NO_BREAK).as_deref() != Some("failed")
        || state_of(HEALTH).as_deref() != Some("passed")
    {
        bail!(
            "the verify evidence should show `{NO_BREAK}` failed and `{HEALTH}` passed: {}",
            deployment["checks"]
        );
    }
    Ok(())
}

fn gate_deployment(task: &Value) -> Option<&Value> {
    task["deploy_gates"]
        .as_array()?
        .iter()
        .find(|g| g["environment"] == ENVIRONMENT)
        .map(|g| &g["deployment"])
}

fn newest_deployment(api: &Api, project: &str) -> Result<Value> {
    let page = api.get(&format!(
        "/api/v1/projects/{project}/deployments?environment={ENVIRONMENT}&limit=1"
    ))?;
    let summary = page["items"]
        .as_array()
        .and_then(|d| d.first())
        .context("the environment has no deployment")?;
    api.get(&format!("/api/v1/deployments/{}", text(&summary["id"])))
}

pub fn healthy_path(api: &Api, project: &str, spec: &str, target: &Target) -> Result<()> {
    let tasks = api.get(&format!("/api/v1/work-items?spec_id={spec}&limit=200"))?;
    let items = tasks["items"].as_array().cloned().unwrap_or_default();
    if items.is_empty() {
        bail!("the spec has no task to check the deployments of");
    }
    for task in &items {
        let key = text(&task["task_key"]);
        let summary = gate_deployment(task)
            .filter(|d| d["state"] == "succeeded")
            .with_context(|| format!("{key} has no succeeded deployment in {ENVIRONMENT}"))?;
        let deployment = api.get(&format!("/api/v1/deployments/{}", text(&summary["id"])))?;
        let carries = deployment["tasks"]
            .as_array()
            .is_some_and(|t| t.iter().any(|t| t["id"] == task["id"]));
        if !carries {
            bail!(
                "deployment {} does not list {key}: {}",
                text(&deployment["id"]),
                deployment["tasks"]
            );
        }
        let finished = timestamp(&deployment["finished_at"], "the deployment's finished_at")?;
        let done = timestamp(&task["done_at"], "the task's done_at")?;
        if done < finished {
            bail!("{key} was done at {done}, before its deployment finished at {finished}");
        }
        println!(
            "  {key} done after deployment {} at {} ({})",
            text(&deployment["id"]),
            short(text(&deployment["sha"])),
            healthy_evidence(&deployment)?
        );
    }
    let latest = newest_deployment(api, project)?;
    let served = target.version()?;
    if latest["state"] != "succeeded" || served != text(&latest["sha"]) {
        bail!(
            "the target serves {served}, the newest deployment is {} at {}",
            latest["state"],
            latest["sha"]
        );
    }
    println!(
        "  target serves {} from deployment {}",
        short(&served),
        text(&latest["id"])
    );
    Ok(())
}

fn watch(task: &Value, seen: &mut String) -> Result<()> {
    let line = progress(task);
    if line != *seen {
        println!("  {line}");
        seen.clone_from(&line);
    }
    if task["merge_auto"] != true {
        bail!(
            "{} would wait for a merge tap: {}",
            text(&task["task_key"]),
            task["autonomy"]
        );
    }
    Ok(())
}

pub fn recover(
    api: &Api,
    cli: &Cli<'_>,
    github: &GitHub,
    run: &Run<'_>,
    project: &str,
    target: &Target,
) -> Result<()> {
    let allowed = BTreeSet::from([BREAK_DONE_WHEN.to_string()]);
    let title = format!("e2e {}: add the break marker", run.run_id);
    let quick = cli.run(&[
        "spec",
        "quick",
        project,
        "--kind",
        "chore",
        "--title",
        &title,
        "--description",
        BREAK_DESCRIPTION,
        "--done-when",
        BREAK_DONE_WHEN,
        "--ac",
        BREAK_AC,
    ])?;
    let task_id = text(&quick["work_items"][0]["id"]).to_string();
    let key = text(&quick["work_items"][0]["task_key"]).to_string();
    println!("  quick spec {} approved, task {key}", text(&quick["path"]));

    let mut seen = String::new();
    let blocked = wait_within(
        "the break task to be blocked by its failed deployment",
        TASK_LIMIT,
        POLL,
        || {
            answer_permissions(api, cli, project, &allowed)?;
            let task = api.get(&format!("/api/v1/work-items/{task_id}"))?;
            watch(&task, &mut seen)?;
            Ok(
                match (text(&task["state"]), text(&task["blocker"]["code"])) {
                    ("blocked", "deploy_unverified") => Wait::Ready(task),
                    ("blocked", code) => bail!("{key} is blocked by {code}, not by the deployment"),
                    ("done" | "cancelled", _) => {
                        bail!(
                            "{key} ended as {} without a failed deployment",
                            task["state"]
                        )
                    }
                    _ => Wait::Waiting(seen.clone()),
                },
            )
        },
    )?;

    let blocker = &blocked["blocker"];
    let params = &blocker["params"];
    if blocker["primary_action"]["kind"] != "revert"
        || params["environment"] != ENVIRONMENT
        || params["phase"] != "verify"
        || params["code"] != "verify_failed"
    {
        bail!("the blocker is not a revertable verify failure: {blocker}");
    }
    let broken = api.get(&format!(
        "/api/v1/deployments/{}",
        text(&params["deployment_id"])
    ))?;
    broken_evidence(&broken)?;
    let pull = blocked["change_request"]["number"]
        .as_u64()
        .context("the break task has no pull request")?;
    if blocked["change_request"]["state"] != "merged" || !github.pull_state(pull)?.merged {
        bail!("#{pull} of the break task is not merged");
    }
    println!(
        "  #{pull} merged, deployment {} failed in verify at {} ({}): `{NO_BREAK}` failed, `{HEALTH}` passed",
        text(&broken["id"]),
        short(text(&broken["sha"])),
        text(&broken["error"]["code"])
    );
    println!(
        "  {key} blocked deploy_unverified, primary action {}",
        blocker["primary_action"]["kind"]
    );

    api.post(
        &format!("/api/v1/work-items/{task_id}/actions/primary"),
        &json!({
            "blocker_id": blocker["id"],
            "action": "revert",
            "reason": REVERT_REASON,
        }),
    )?;
    println!("  revert requested through the primary action");

    let mut seen = String::new();
    let revert = wait_within("the revert task to be done", REVERT_LIMIT, POLL, || {
        answer_permissions(api, cli, project, &allowed)?;
        let items = api.get(&format!(
            "/api/v1/work-items?project_id={}&limit=200",
            project
        ))?;
        let found = items["items"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|t| t["revert_of"]["id"] == task_id.as_str());
        let Some(task) = found else {
            return Ok(Wait::Waiting("no revert task yet".to_string()));
        };
        watch(task, &mut seen)?;
        Ok(match text(&task["state"]) {
            "done" => Wait::Ready(task.clone()),
            "blocked" | "cancelled" | "failed" => {
                bail!(
                    "the revert task {} is {}: {}",
                    text(&task["task_key"]),
                    task["state"],
                    task["blocker"]
                )
            }
            _ => Wait::Waiting(seen.clone()),
        })
    })?;
    let revert_key = text(&revert["task_key"]);
    if revert_key != format!("{key}-R1") {
        bail!("the revert task is {revert_key}, expected {key}-R1");
    }
    let revert_pull = revert["change_request"]["number"]
        .as_u64()
        .context("the revert task has no pull request")?;
    if !github.pull_state(revert_pull)?.merged {
        bail!("the revert #{revert_pull} is not merged on GitHub");
    }
    let origin = api.get(&format!("/api/v1/work-items/{task_id}"))?;
    if origin["state"] != "cancelled" {
        bail!(
            "{key} is {}, expected cancelled after its revert",
            origin["state"]
        );
    }

    let recovered = newest_deployment(api, project)?;
    if recovered["state"] != "succeeded"
        || recovered["id"] == broken["id"]
        || recovered["sha"] == broken["sha"]
    {
        bail!(
            "the deployment after the revert is {} at {}, not a new succeeded one",
            recovered["state"],
            recovered["sha"]
        );
    }
    let carries = recovered["tasks"]
        .as_array()
        .is_some_and(|t| t.iter().any(|t| t["id"] == revert["id"]));
    if !carries {
        bail!(
            "deployment {} does not list {revert_key}: {}",
            text(&recovered["id"]),
            recovered["tasks"]
        );
    }
    let evidence = healthy_evidence(&recovered)?;
    let served = target.version()?;
    if served != text(&recovered["sha"]) {
        bail!(
            "the target serves {served}, the recovered deployment is at {}",
            recovered["sha"]
        );
    }
    println!(
        "  revert #{revert_pull} (Revert of #{pull}) merged by Saka; deployment {} succeeded at {} ({evidence})",
        text(&recovered["id"]),
        short(&served)
    );
    println!(
        "  {key} {}, {revert_key} {}",
        origin["state"], revert["state"]
    );
    println!("  skip redo (not shipped)");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOGFOOD: &str = "[project]\nname = \"saka-dogfood\"\n\n[[repo]]\nname = \"app\"\nurl = \"https://github.com/acme/app\"\n\n[workflow.issue]\ntrigger = \"label\"\ntrigger_label = \"saka-e2e-r1\"\nmax_open = 20\n";

    fn started() -> (tempfile::TempDir, Target) {
        let root = tempfile::tempdir().unwrap_or_else(|e| panic!("tempdir: {e}"));
        let target =
            Target::start(&root.path().join("deploy")).unwrap_or_else(|e| panic!("start: {e:#}"));
        (root, target)
    }

    fn parsed(text: &str) -> toml::Table {
        text.parse().unwrap_or_default()
    }

    #[test]
    fn the_run_environment_names_the_project_the_target_and_the_revert_recovery() {
        let (_root, target) = started();
        let block = environment_block("saka-dogfood", &target);
        let table = parsed(&block);
        let deploy = &table["deploy"][0];
        assert_eq!(deploy["project"].as_str(), Some("saka-dogfood"));
        assert_eq!(deploy["environment"].as_str(), Some(ENVIRONMENT));
        assert_eq!(deploy["action"].as_str(), Some("run_host"));
        assert_eq!(deploy["command"].as_str(), Some(DEPLOY_COMMAND));
        assert_eq!(deploy["gate"].as_bool(), Some(true));
        assert_eq!(deploy["on_failure"].as_str(), Some("revert"));
        assert_eq!(deploy["timeout_secs"].as_integer(), Some(60));
        assert_eq!(
            deploy["target_url"].as_str(),
            Some(format!("http://127.0.0.1:{}", target.port()).as_str())
        );
        assert_eq!(
            deploy["cwd"].as_str(),
            Some(target.dir().to_string_lossy().as_ref())
        );
    }

    #[test]
    fn the_verify_table_is_added_and_the_rest_of_the_file_is_kept() {
        let out = with_verify(DOGFOOD).unwrap_or_default();
        let table = parsed(&out);
        let verify = &table["verify"][ENVIRONMENT];
        assert_eq!(verify["ready"].as_str(), Some(READY));
        assert_eq!(verify["ready_interval_secs"].as_integer(), Some(5));
        assert_eq!(verify["ready_timeout_secs"].as_integer(), Some(120));
        let commands: Vec<&str> = verify["commands"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(toml::Value::as_str)
            .collect();
        assert_eq!(commands, [HEALTH, NO_BREAK]);
        assert_eq!(
            table["workflow"]["issue"]["trigger_label"].as_str(),
            Some("saka-e2e-r1")
        );
        assert_eq!(table["repo"][0]["name"].as_str(), Some("app"));
        assert!(out.ends_with("]\n"));
    }

    #[test]
    fn a_file_that_already_verifies_something_is_refused() {
        let existing = format!("{DOGFOOD}\n[verify.staging]\ncommands = []\n");
        assert!(with_verify(&existing).is_err());
    }

    #[test]
    fn the_project_name_comes_from_the_project_table() {
        assert_eq!(project_name(DOGFOOD).unwrap_or_default(), "saka-dogfood");
        assert!(project_name("[workflow]\n").is_err());
    }

    #[test]
    fn the_target_has_no_version_until_a_deploy_writes_one_and_is_always_healthy() {
        let (_root, target) = started();
        assert_eq!(
            target.get("/health").unwrap_or_default(),
            (200, "ok".into())
        );
        assert_eq!(target.get("/version").unwrap_or_default().0, 404);
        assert!(target.version().is_err());
        std::fs::write(target.dir().join("sha"), "abc123").unwrap_or_else(|e| panic!("write: {e}"));
        assert_eq!(target.version().unwrap_or_default(), "abc123");
        assert_eq!(target.get("/other").unwrap_or_default().0, 404);
    }

    #[test]
    fn stopping_the_target_removes_its_directory() {
        let (_root, target) = started();
        let dir = target.dir().to_path_buf();
        assert!(dir.is_dir());
        drop(target);
        assert!(!dir.exists());
    }

    fn check(phase: &str, command: Option<&str>, state: &str) -> Value {
        json!({ "phase": phase, "command": command, "state": state, "attempts": 2 })
    }

    fn deployment(state: &str, code: &str, verify: [&str; 2]) -> Value {
        json!({
            "id": "d1",
            "state": state,
            "phase": "verify",
            "error": { "code": code },
            "checks": [
                check("deploy", None, "passed"),
                check("ready", Some(READY), "passed"),
                check("verify", Some(HEALTH), verify[0]),
                check("verify", Some(NO_BREAK), verify[1]),
            ],
        })
    }

    #[test]
    fn a_healthy_deployment_needs_every_phase_to_pass() {
        let good = deployment("succeeded", "", ["passed", "passed"]);
        assert!(
            healthy_evidence(&good)
                .unwrap_or_default()
                .contains("2 verify checks")
        );
        let bad = deployment("failed", "verify_failed", ["passed", "failed"]);
        assert!(healthy_evidence(&bad).is_err());
    }

    #[test]
    fn a_broken_deployment_fails_in_verify_on_the_marker_check_only() {
        let broken = deployment("failed", "verify_failed", ["passed", "failed"]);
        assert!(broken_evidence(&broken).is_ok());
        let wrong_check = deployment("failed", "verify_failed", ["failed", "passed"]);
        assert!(broken_evidence(&wrong_check).is_err());
        let wrong_code = deployment("failed", "ready_timed_out", ["passed", "failed"]);
        assert!(broken_evidence(&wrong_code).is_err());
        let healthy = deployment("succeeded", "", ["passed", "passed"]);
        assert!(broken_evidence(&healthy).is_err());
    }

    #[test]
    fn the_gate_of_the_run_environment_is_the_one_read_from_a_task() {
        let task = json!({ "deploy_gates": [
            { "environment": "other", "deployment": { "id": "x" } },
            { "environment": ENVIRONMENT, "deployment": { "id": "y" } },
        ]});
        assert_eq!(gate_deployment(&task).map(|d| text(&d["id"])), Some("y"));
        assert!(gate_deployment(&json!({ "deploy_gates": [] })).is_none());
    }
}
