use std::collections::BTreeSet;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde_json::Value;

use super::github::GitHub;
use super::{Api, Cli, POLL, Run, answer_permissions, progress, short, text};

pub const PROJECT_PATH: &str = "docs-review";
const OUTSIDE_LIMIT: Duration = Duration::from_secs(10 * 60);
const HOLD: Duration = Duration::from_secs(20);

fn head_branch(run_id: &str) -> String {
    format!("e2e-ext/{run_id}")
}

pub fn saka_toml(repo: &str, run_id: &str) -> String {
    format!(
        "[project]\nname = \"saka-dogfood-review\"\n\n[[repo]]\nname = \"app\"\nurl = \"https://github.com/{repo}\"\n\n[workflow]\npreset = \"review_only\"\n\n[workflow.change_request]\nbranches = [\"{}\"]\n",
        head_branch(run_id)
    )
}

// The pull request needs an author other than the GitHub App: the provider refuses a verdict
// from the account that opened the pull request.
pub(super) fn author_token() -> Result<String> {
    let token = match std::env::var("SAKA_E2E_AUTHOR_TOKEN_FILE") {
        Ok(file) if !file.is_empty() => {
            std::fs::read_to_string(&file).with_context(|| format!("read {file}"))?
        }
        _ => {
            let out = Command::new("gh")
                .args(["auth", "token"])
                .stderr(Stdio::null())
                .output()
                .context("run `gh auth token` (or set SAKA_E2E_AUTHOR_TOKEN_FILE)")?;
            if !out.status.success() {
                bail!("`gh auth token` failed: log in with gh or set SAKA_E2E_AUTHOR_TOKEN_FILE");
            }
            String::from_utf8_lossy(&out.stdout).into_owned()
        }
    };
    let token = token.trim().to_string();
    if token.is_empty() {
        bail!("the author token is empty");
    }
    Ok(token)
}

fn review_state(event: &str) -> &'static str {
    match event {
        "approve" => "APPROVED",
        "request_changes" => "CHANGES_REQUESTED",
        _ => "COMMENTED",
    }
}

fn outside_task<'a>(items: &'a Value, number: u64, branch: &str) -> Option<&'a Value> {
    items["items"].as_array()?.iter().find(|t| {
        t["change_request"]["number"] == number
            || (t["origin"] == "external_change" && t["branch"] == branch)
    })
}

pub fn review_only(api: &Api, cli: &Cli<'_>, run: &Run<'_>) -> Result<()> {
    let repo = run.settings.repo.as_str();
    let url = format!("https://github.com/{repo}");
    let project = cli.run(&[
        "project",
        "add",
        &url,
        "--path",
        PROJECT_PATH,
        "--base-branch",
        &run.base,
    ])?;
    let project_id = text(&project["id"]).to_string();
    if project["workflow"]["preset"] != "review_only" {
        bail!(
            "the second project is not review_only: {}",
            project["workflow"]
        );
    }

    let author = GitHub::with_token(author_token()?, repo)?;
    let branch = head_branch(&run.run_id);
    author.create_branch(&branch, &author.branch_sha(&run.base)?)?;
    author.put_file(
        &branch,
        &format!("e2e-runs/{}/outside.md", run.run_id),
        "A change that arrived from outside Saka.\n",
        "docs: add an outside change",
    )?;
    let number = author.open_pull(
        &branch,
        &run.base,
        &format!("Outside change {}", run.run_id),
        "Opened by the end-to-end run, not by Saka.",
    )?;
    let head_sha = author.pull_state(number)?.head_sha;
    println!("  outside pull request #{number} at {}", short(&head_sha));

    let started = Instant::now();
    let mut seen = String::new();
    let task = loop {
        if started.elapsed() > OUTSIDE_LIMIT {
            bail!(
                "no review was posted on #{number} within {} minutes",
                OUTSIDE_LIMIT.as_secs() / 60
            );
        }
        answer_permissions(api, cli, &project_id, &BTreeSet::new())?;
        let items = api.get(&format!(
            "/api/v1/work-items?project_id={project_id}&limit=200"
        ))?;
        if let Some(task) = outside_task(&items, number, &branch) {
            let line = progress(task);
            if line != seen {
                println!("  {line}");
                seen = line;
            }
            if task["state"] == "blocked" {
                bail!(
                    "{} is blocked: {}",
                    text(&task["task_key"]),
                    task["blocker"]
                );
            }
            let review = &task["review"];
            if !review["posted"].is_null() && review["head_sha"] == head_sha.as_str() {
                break task.clone();
            }
        }
        std::thread::sleep(POLL);
    };

    if task["origin"] != "external_change"
        || task["external"]["cause"] != "auto"
        || !task["external"]["spec"].is_null()
    {
        bail!("the outside pull request was not taken as expected: {task}");
    }
    let event = text(&task["review"]["posted"]["event"]);
    let expected = review_state(event);
    let reviews = author.reviews(number)?;
    if !reviews
        .iter()
        .any(|r| r.bot && r.commit_id == head_sha && r.state == expected)
    {
        bail!(
            "GitHub has no {expected} review of Saka on #{number} at {head_sha}; it has {:?}",
            reviews
                .iter()
                .map(|r| format!("{} {} bot={}", short(&r.commit_id), r.state, r.bot))
                .collect::<Vec<_>>()
        );
    }
    println!(
        "  Saka reviewed #{number} at {}: {} ({expected} on GitHub)",
        short(&head_sha),
        text(&task["review"]["outcome"])
    );

    let settled = Instant::now();
    while !author.checks_done(&head_sha)? || settled.elapsed() < HOLD {
        if settled.elapsed() > OUTSIDE_LIMIT {
            bail!("the checks of #{number} did not finish");
        }
        std::thread::sleep(POLL);
    }
    let state = author.pull_state(number)?;
    let items = api.get(&format!(
        "/api/v1/work-items?project_id={project_id}&limit=200"
    ))?;
    let task = outside_task(&items, number, &branch).context("the outside task disappeared")?;
    if state.merged || !state.open || task["merge_auto"] == true || task["state"] == "done" {
        bail!(
            "Saka must not merge an outside pull request on its own: open {}, merged {}, merge_auto {}, state {}",
            state.open,
            state.merged,
            task["merge_auto"],
            task["state"]
        );
    }
    println!("  #{number} still open and unmerged after its checks finished");
    author.close_pull(number)?;
    author.delete_branch(&branch)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn the_review_only_config_takes_only_this_runs_branch() {
        let text = saka_toml("acme/app", "r1");
        let parsed: toml::Table = text.parse().unwrap_or_default();
        assert_eq!(parsed["workflow"]["preset"].as_str(), Some("review_only"));
        let branches = &parsed["workflow"]["change_request"]["branches"];
        assert_eq!(branches[0].as_str(), Some("e2e-ext/r1"));
        assert_eq!(
            parsed["repo"][0]["url"].as_str(),
            Some("https://github.com/acme/app")
        );
    }

    #[test]
    fn the_outside_task_is_found_by_number_or_branch() {
        let items = json!({ "items": [
            { "origin": "spec_step", "branch": "saka/x/s1", "change_request": { "number": 3 } },
            { "origin": "external_change", "branch": "e2e-ext/r1", "change_request": null },
        ]});
        assert!(outside_task(&items, 3, "e2e-ext/r1").is_some());
        assert_eq!(
            outside_task(&items, 9, "e2e-ext/r1").map(|t| t["origin"].clone()),
            Some(json!("external_change"))
        );
        assert!(outside_task(&items, 9, "other").is_none());
    }

    #[test]
    fn a_posted_event_maps_to_the_state_github_shows() {
        assert_eq!(review_state("approve"), "APPROVED");
        assert_eq!(review_state("request_changes"), "CHANGES_REQUESTED");
        assert_eq!(review_state("comment"), "COMMENTED");
    }
}
