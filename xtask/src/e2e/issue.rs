use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde_json::Value;

use super::github::GitHub;
use super::{Api, Cli, Run, answer_permissions, outside, progress, short, text};

const SAKA_TOML: &str = "docs/saka.toml";
const LABEL_COLOR: &str = "0e8a16";
const LABEL_DESCRIPTION: &str = "end-to-end run";
const BUGFIX_TABLE: &str = "[workflow.issue.bugfix]";
const ISSUE_TABLE: &str = "[workflow.issue]";
const BUGFIX_BODY: &str = "done_when = [\"go -C cli vet ./...\", \"go -C cli test ./...\"]\nregression = [\"go -C cli test ./...\"]\n";
const DONE_WHEN: [&str; 2] = ["go -C cli vet ./...", "go -C cli test ./..."];
const BUG_SOURCE_PATH: &str = "cli/internal/e2ebug/clamp.go";
const BUG_TEST_PATH: &str = "cli/internal/e2ebug/clamp_test.go";
const BUG_SOURCE: &str = "package e2ebug\n\nfunc Clamp(v, lo, hi int) int {\n\tif v < lo {\n\t\treturn lo\n\t}\n\tif v > hi {\n\t\treturn lo\n\t}\n\treturn v\n}\n";
const BUG_TEST: &str = "package e2ebug\n\nimport \"testing\"\n\nfunc TestClampKeepsAValueInsideTheRange(t *testing.T) {\n\tif got := Clamp(5, 0, 10); got != 5 {\n\t\tt.Fatalf(\"Clamp(5, 0, 10) = %d, want 5\", got)\n\t}\n}\n\nfunc TestClampRaisesAValueBelowTheRange(t *testing.T) {\n\tif got := Clamp(-3, 0, 10); got != 0 {\n\t\tt.Fatalf(\"Clamp(-3, 0, 10) = %d, want 0\", got)\n\t}\n}\n";
const ISSUE_BODY: &str = "`Clamp` in `cli/internal/e2ebug/clamp.go` returns the lower bound for a value above the range.\n\nSteps to reproduce:\n\n1. Call `Clamp(15, 0, 10)`.\n\nExpected: `10`, the upper bound.\nActual: `0`, the lower bound.\n\nValues inside the range and below it are already right.\n";
const ISSUE_LIMIT_VARIABLE: &str = "SAKA_E2E_ISSUE_LIMIT_SECS";
const INTAKE_LIMIT: Duration = Duration::from_secs(5 * 60);
const COMMENT_LIMIT: Duration = Duration::from_secs(2 * 60);
const TASK_LIMIT: Duration = Duration::from_secs(25 * 60);
const DONE_LIMIT: Duration = Duration::from_secs(2 * 60);
const CLOSE_LIMIT: Duration = Duration::from_secs(3 * 60);
const GITHUB_POLL: Duration = Duration::from_secs(5);
const FAILED_COMMENT: &str = "end-to-end run failed, see serve log";

pub(super) enum Wait<T> {
    Ready(T),
    Waiting(String),
}

fn limit(default: Duration) -> Duration {
    std::env::var(ISSUE_LIMIT_VARIABLE)
        .ok()
        .and_then(|v| v.parse().ok())
        .map_or(default, Duration::from_secs)
}

fn wait<T>(
    what: &str,
    default: Duration,
    pause: Duration,
    step: impl FnMut() -> Result<Wait<T>>,
) -> Result<T> {
    wait_within(what, limit(default), pause, step)
}

pub(super) fn wait_within<T>(
    what: &str,
    allowed: Duration,
    pause: Duration,
    mut step: impl FnMut() -> Result<Wait<T>>,
) -> Result<T> {
    let started = Instant::now();
    loop {
        let last = match step()? {
            Wait::Ready(value) => return Ok(value),
            Wait::Waiting(last) => last,
        };
        if started.elapsed() > allowed {
            bail!(
                "timed out after {}s waiting for {what}; last state: {last}",
                allowed.as_secs()
            );
        }
        std::thread::sleep(pause);
    }
}

pub fn label_name(run_id: &str) -> String {
    format!("saka-e2e-{run_id}")
}

pub fn issue_title(run_id: &str) -> String {
    format!("e2e {run_id}: Clamp returns the lower bound above the range")
}

pub fn rewrite_saka_toml(original: &str, label: &str) -> Result<String> {
    let mut out = String::new();
    let mut table = String::new();
    let mut labelled = false;
    let mut bugfix = false;
    for line in original.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            table = trimmed.to_string();
            if table == BUGFIX_TABLE {
                bugfix = true;
                out.push_str(&format!("{line}\n{BUGFIX_BODY}\n"));
                continue;
            }
        } else if table == BUGFIX_TABLE {
            continue;
        }
        let is_label = table == ISSUE_TABLE
            && trimmed
                .strip_prefix("trigger_label")
                .is_some_and(|rest| rest.trim_start().starts_with('='));
        if is_label {
            labelled = true;
            out.push_str(&format!("trigger_label = {}\n", super::toml_string(label)));
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    if !labelled {
        bail!("{SAKA_TOML} has no trigger_label under {ISSUE_TABLE}");
    }
    let mut out = out.trim_end().to_string();
    out.push('\n');
    if !bugfix {
        out.push_str(&format!("\n{BUGFIX_TABLE}\n{BUGFIX_BODY}"));
    }
    Ok(out)
}

pub fn intake_of(items: &Value, number: u64) -> Option<&Value> {
    items["items"]
        .as_array()?
        .iter()
        .find(|i| i["issue"]["number"] == number)
}

pub fn prepare_base(github: &GitHub, base: &str, label: &str) -> Result<()> {
    let (original, blob) = github.file(base, SAKA_TOML)?;
    github.update_file(
        base,
        SAKA_TOML,
        &rewrite_saka_toml(&original, label)?,
        "docs: scope the issue intake to this run",
        &blob,
    )?;
    github.put_file(base, BUG_SOURCE_PATH, BUG_SOURCE, "feat: add a range clamp")?;
    github.put_file(
        base,
        BUG_TEST_PATH,
        BUG_TEST,
        "test: cover the range clamp inside and below the range",
    )
}

fn intake_state(api: &Api, project: &str, number: u64) -> Result<Option<Value>> {
    let items = api.get(&format!(
        "/api/v1/projects/{project}/issue-intakes?limit=100"
    ))?;
    Ok(intake_of(&items, number).cloned())
}

fn wait_for_comment(github: &GitHub, number: u64, what: &str, needle: &str) -> Result<String> {
    wait(
        &format!("the {what} comment on #{number}"),
        COMMENT_LIMIT,
        GITHUB_POLL,
        || {
            let comments = github.issue_comments(number)?;
            Ok(
                match comments.iter().find(|c| c.bot && c.body.contains(needle)) {
                    Some(found) => Wait::Ready(found.body.clone()),
                    None => Wait::Waiting(format!("{} comment(s), none matching", comments.len())),
                },
            )
        },
    )
}

pub fn bugfix_from_issue(
    api: &Api,
    cli: &Cli<'_>,
    github: &GitHub,
    run: &Run<'_>,
    project: &str,
) -> Result<()> {
    let author = GitHub::with_token(outside::author_token()?, run.settings.repo.as_str())?;
    let label = label_name(&run.run_id);
    author.create_label(&label, LABEL_COLOR, LABEL_DESCRIPTION)?;
    let mut number = None;
    let outcome = scenario(api, cli, github, &author, run, project, &mut number);
    let cleaned = cleanup(&author, number, &label, outcome.is_ok());
    match (outcome, cleaned) {
        (Ok(()), cleaned) => cleaned,
        (Err(e), cleaned) => {
            if let Err(c) = cleaned {
                eprintln!("  cleanup of the issue scenario failed: {c:#}");
            }
            Err(e)
        }
    }
}

fn cleanup(author: &GitHub, number: Option<u64>, label: &str, passed: bool) -> Result<()> {
    if let Some(number) = number
        && !passed
        && author.issue(number)?.open
    {
        author.comment_on_issue(number, FAILED_COMMENT)?;
        author.close_issue_not_planned(number)?;
        println!("  closed issue #{number} as not planned");
    }
    author.delete_label(label)?;
    println!("  deleted label {label}");
    Ok(())
}

fn scenario(
    api: &Api,
    cli: &Cli<'_>,
    github: &GitHub,
    author: &GitHub,
    run: &Run<'_>,
    project: &str,
    created: &mut Option<u64>,
) -> Result<()> {
    let label = label_name(&run.run_id);
    let number = author.create_issue(&issue_title(&run.run_id), ISSUE_BODY, &["bug", &label])?;
    *created = Some(number);
    println!("  issue #{number} labelled bug and {label}");

    let intake = wait(
        &format!("the intake of issue #{number} to await approval"),
        INTAKE_LIMIT,
        super::POLL,
        || {
            let Some(found) = intake_state(api, project, number)? else {
                return Ok(Wait::Waiting("no intake yet".to_string()));
            };
            match text(&found["state"]) {
                "awaiting_approval" => Ok(Wait::Ready(found)),
                "failed" | "dismissed" | "closed" => {
                    bail!("the intake of issue #{number} ended early: {found}")
                }
                state => Ok(Wait::Waiting(state.to_string())),
            }
        },
    )?;
    if intake["lane"] != "bugfix" || intake["kind"] != "bugfix" {
        bail!(
            "the intake is not a bugfix: lane {} kind {}",
            intake["lane"],
            intake["kind"]
        );
    }
    let spec_id = text(&intake["spec"]["id"]).to_string();
    let spec_path = text(&intake["spec"]["path"]).to_string();
    let wanted = format!("-{number}.md");
    if !spec_path.contains("specs/bugfix-") || !spec_path.ends_with(&wanted) {
        bail!("the intake spec path {spec_path} is not specs/bugfix-…{wanted}");
    }
    println!("  intake awaits approval with {spec_path}");
    wait_for_comment(github, number, "accepted", "picked this up")?;
    wait_for_comment(github, number, "awaiting approval", "waits for approval")?;

    let spec = api.get(&format!("/api/v1/specs/{spec_id}"))?;
    if spec["origin"] != "intake" || spec["kind"] != "bugfix" {
        bail!(
            "the spec is not a bugfix from the intake: origin {} kind {}",
            spec["origin"],
            spec["kind"]
        );
    }
    let (content, _) = github.file(&run.base, &spec_path)?;
    if !content.contains("## Regression") {
        bail!("{spec_path} has no ## Regression section");
    }
    cli.touch(
        "approve the issue spec".to_string(),
        &[
            "spec",
            "approve",
            project,
            &spec_path,
            "--reason",
            "end-to-end run",
        ],
    )?;
    println!("  approved {spec_path}");

    let allowed: BTreeSet<String> = DONE_WHEN.iter().map(|c| (*c).to_string()).collect();
    let mut seen = String::new();
    let task = wait("the bugfix task to ship", TASK_LIMIT, super::POLL, || {
        answer_permissions(api, cli, project, &allowed)?;
        let tasks = api.get(&format!("/api/v1/work-items?spec_id={spec_id}&limit=200"))?;
        let items = tasks["items"].as_array().cloned().unwrap_or_default();
        for task in &items {
            let line = progress(task);
            if line != seen {
                println!("  {line}");
                seen = line;
            }
            if task["merge_auto"] != true {
                bail!(
                    "the bugfix task would wait for a merge tap: {}",
                    task["autonomy"]
                );
            }
            if text(&task["state"]) == "blocked" {
                bail!("the bugfix task is blocked: {}", task["blocker"]);
            }
        }
        let shipped = text(&api.get(&format!("/api/v1/specs/{spec_id}"))?["state"]) == "shipped";
        match (shipped, items.as_slice()) {
            (true, [task]) => Ok(Wait::Ready(task.clone())),
            (true, other) => bail!("the bugfix spec has {} tasks, expected one", other.len()),
            (false, _) => Ok(Wait::Waiting(seen.clone())),
        }
    })?;
    println!("  spec {spec_path} shipped");

    let regression = &task["regression"];
    let proof = text(&regression["commit_sha"]);
    let commands = regression["commands"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if regression["state"] != "reproduced"
        || proof.is_empty()
        || commands.is_empty()
        || commands.iter().any(|c| c["result"] != "failed")
    {
        bail!("the test-first proof is missing or incomplete: {regression}");
    }
    let change = &task["change_request"];
    let head = text(&change["head_sha"]);
    if proof == head {
        bail!(
            "the regression was reproduced at the pull request head {head}, so no fix came after the test"
        );
    }
    if !task["gates"]
        .as_array()
        .is_some_and(|g| g.iter().any(|g| g["outcome"] == "passed"))
    {
        bail!("no gate passed at the pull request head: {}", task["gates"]);
    }
    let pull = change["number"]
        .as_u64()
        .context("the bugfix task has no pull request")?;
    if change["state"] != "merged" || text(&change["base_branch"]) != run.base {
        bail!(
            "the pull request #{pull} did not merge into {}: {change}",
            run.base
        );
    }
    let body = author.pull_body(pull)?;
    if !body.contains(&format!("Closes #{number}")) {
        bail!("pull request #{pull} does not say Closes #{number}");
    }
    println!(
        "  regression reproduced at {} ({} command(s) failed), gate passed at {}, #{pull} merged by Saka with Closes #{number}",
        short(proof),
        commands.len(),
        short(head)
    );

    wait(
        &format!("the intake of issue #{number} to be done"),
        DONE_LIMIT,
        super::POLL,
        || {
            let state = intake_state(api, project, number)?
                .map(|i| text(&i["state"]).to_string())
                .unwrap_or_default();
            Ok(if state == "done" {
                Wait::Ready(())
            } else {
                Wait::Waiting(state)
            })
        },
    )?;
    wait_for_comment(
        github,
        number,
        "done",
        &format!("is merged into `{}`", run.base),
    )?;

    let closed = wait(
        &format!("issue #{number} to be closed"),
        CLOSE_LIMIT,
        GITHUB_POLL,
        || {
            let issue = github.issue(number)?;
            Ok(if issue.open {
                Wait::Waiting("open".to_string())
            } else {
                Wait::Ready(issue)
            })
        },
    )?;
    if closed.state_reason != "completed" || !closed.closed_by_bot {
        bail!(
            "issue #{number} was closed with reason {:?} by {:?} (bot: {}), expected completed by the App",
            closed.state_reason,
            closed.closed_by,
            closed.closed_by_bot
        );
    }
    println!(
        "  issue #{number} closed ({}) by {}",
        closed.state_reason, closed.closed_by
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOGFOOD: &str = "[project]\nname = \"saka-dogfood\"\n\n[[repo]]\nname = \"app\"\nurl = \"https://github.com/acme/app\"\n\n[workflow.issue]\ntrigger = \"label\"\ntrigger_label = \"saka:intake\"\nexclude_labels = [\"wontfix\"]\nkind_labels = { bug = \"bugfix\" }\nmax_open = 20\n\n[workflow.issue.bugfix]\ndone_when = [\"go -C cli vet ./...\", \"npm --prefix web test\"]\nregression = [\"npm --prefix web test\"]\n";

    fn parsed(text: &str) -> toml::Table {
        text.parse().unwrap_or_default()
    }

    #[test]
    fn the_run_label_comes_from_the_run_id() {
        assert_eq!(
            label_name("20261113-101010-1a"),
            "saka-e2e-20261113-101010-1a"
        );
        assert!(issue_title("r1").starts_with("e2e r1: "));
    }

    #[test]
    fn the_rewrite_swaps_the_trigger_label_and_the_bugfix_commands_and_keeps_the_rest() {
        let out = rewrite_saka_toml(DOGFOOD, "saka-e2e-r1").unwrap_or_default();
        let table = parsed(&out);
        let issue = &table["workflow"]["issue"];
        assert_eq!(issue["trigger_label"].as_str(), Some("saka-e2e-r1"));
        assert_eq!(issue["trigger"].as_str(), Some("label"));
        assert_eq!(issue["max_open"].as_integer(), Some(20));
        assert_eq!(issue["kind_labels"]["bug"].as_str(), Some("bugfix"));
        let bugfix = &issue["bugfix"];
        let commands = |key: &str| -> Vec<String> {
            bugfix[key]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|c| c.as_str().map(String::from))
                .collect()
        };
        assert_eq!(commands("done_when"), DONE_WHEN);
        assert_eq!(commands("regression"), ["go -C cli test ./..."]);
        assert_eq!(table["project"]["name"].as_str(), Some("saka-dogfood"));
        assert_eq!(
            table["repo"][0]["url"].as_str(),
            Some("https://github.com/acme/app")
        );
        assert!(!out.contains("saka:intake") && !out.contains("npm"));
        assert!(out.ends_with("\"go -C cli test ./...\"]\n"));
    }

    #[test]
    fn a_file_without_a_bugfix_table_gets_one_and_a_file_without_a_label_is_refused() {
        let bare = "[workflow.issue]\ntrigger_label = \"x\"\n";
        let out = rewrite_saka_toml(bare, "l").unwrap_or_default();
        assert_eq!(
            parsed(&out)["workflow"]["issue"]["bugfix"]["regression"][0].as_str(),
            Some("go -C cli test ./...")
        );
        assert!(rewrite_saka_toml("[workflow.issue]\nmax_open = 1\n", "l").is_err());
    }

    #[test]
    fn an_intake_is_found_by_its_issue_number() {
        let items = serde_json::json!({ "items": [
            { "issue": { "number": 4 }, "state": "done" },
            { "issue": { "number": 9 }, "state": "accepted" },
        ]});
        assert_eq!(
            intake_of(&items, 9).map(|i| text(&i["state"])),
            Some("accepted")
        );
        assert!(intake_of(&items, 5).is_none());
    }

    #[test]
    fn the_planted_bug_passes_its_own_test_and_fails_above_the_range() {
        assert!(BUG_TEST.contains("Clamp(5, 0, 10)") && BUG_TEST.contains("Clamp(-3, 0, 10)"));
        assert!(!BUG_TEST.contains("Clamp(15"));
        assert!(ISSUE_BODY.contains("Clamp(15, 0, 10)"));
    }
}
