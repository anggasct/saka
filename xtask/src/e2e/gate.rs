use anyhow::{Context, Result, bail};
use serde_json::Value;

use super::github::GitHub;
use super::{Api, Cli, TOUCH_LIMIT, short, text};

pub fn check(api: &Api, cli: &Cli<'_>, github: &GitHub, spec: &str) -> Result<()> {
    auto_merge(api, spec)?;
    touches(cli)?;
    fix_round(api, github, spec)
}

fn touches(cli: &Cli<'_>) -> Result<()> {
    let touches = cli.touches.borrow();
    println!(
        "  {} operator action(s) from approve to shipped: {}",
        touches.len(),
        touches.join("; ")
    );
    let prompts = cli.prompts.borrow();
    println!(
        "  {} tool permission(s) answered, not counted as touches: {}",
        prompts.len(),
        prompts.join("; ")
    );
    if touches.len() > TOUCH_LIMIT {
        bail!(
            "the spec needed {} operator actions, the limit is {TOUCH_LIMIT}: {}",
            touches.len(),
            touches.join("; ")
        );
    }
    Ok(())
}

fn counter(metrics: &str, name: &str, by: &str) -> u64 {
    let prefix = format!("{name}{{by=\"{by}\"}} ");
    metrics
        .lines()
        .find_map(|l| l.strip_prefix(&prefix))
        .and_then(|n| n.trim().parse().ok())
        .unwrap_or(0)
}

fn auto_merge(api: &Api, spec: &str) -> Result<()> {
    let tasks = api.get(&format!("/api/v1/work-items?spec_id={spec}&limit=200"))?;
    for task in tasks["items"].as_array().into_iter().flatten() {
        if task["change_request"]["state"] != "merged" || task["merge_auto"] != true {
            bail!(
                "{} did not merge on its own: {} merge_auto {}",
                text(&task["task_key"]),
                task["change_request"]["state"],
                task["merge_auto"]
            );
        }
    }
    let metrics = api.get_text("/metrics")?;
    let name = "saka_merge_requests_total";
    let (auto, operator) = (
        counter(&metrics, name, "autonomy"),
        counter(&metrics, name, "operator"),
    );
    if auto == 0 || operator != 0 {
        bail!(
            "expected merges requested by autonomy and none by an operator, got {auto} and {operator}"
        );
    }
    println!("  {auto} merge(s) requested by autonomy, {operator} by an operator");
    Ok(())
}

fn fix_round(api: &Api, github: &GitHub, spec: &str) -> Result<()> {
    let tasks = api.get(&format!("/api/v1/work-items?spec_id={spec}&limit=200"))?;
    let step = tasks["items"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|t| t["step_id"] == "S2")
        .context("S2 is missing")?;
    let detail = api.get(&format!("/api/v1/work-items/{}", text(&step["id"])))?;
    let mut reviews: Vec<&Value> = detail["reviews"].as_array().into_iter().flatten().collect();
    reviews.reverse();
    let (Some(first), Some(last)) = (reviews.first(), reviews.last()) else {
        bail!("S2 has no review: {detail}");
    };
    if first["outcome"] != "changes_requested" {
        bail!(
            "the fix path was not exercised: the reviewer judged the first head of S2 as {} although s2.md did not quote s1.md: {}",
            first["outcome"],
            first["summary"]
        );
    }
    let round = detail["fix"]["round"].as_u64().unwrap_or(0);
    let ran_fix = detail["stage_runs"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|r| r["stage"] == "fix");
    if round == 0 || !ran_fix {
        bail!(
            "S2 had a changes_requested verdict but no fix round ran: {}",
            detail["fix"]
        );
    }
    if last["outcome"] != "approved"
        || last["current"] != true
        || last["head_sha"] == first["head_sha"]
    {
        bail!("S2 was not approved at a new head after its fix: first {first}, last {last}",);
    }
    let number = detail["change_request"]["number"]
        .as_u64()
        .context("S2 has no pull request")?;
    let posted = github.reviews(number)?;
    for (what, head) in [("first", first), ("last", last)] {
        let sha = text(&head["head_sha"]);
        if !posted.iter().any(|r| r.bot && r.commit_id == sha) {
            bail!("no review of Saka on #{number} at the {what} verdict's head {sha}");
        }
    }
    println!(
        "  S2 fix round {round}: changes_requested at {}, approved at {}, #{number} merged",
        short(text(&first["head_sha"])),
        short(text(&last["head_sha"]))
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::counter;

    #[test]
    fn a_counter_is_read_by_label_and_absent_means_zero() {
        let metrics = "# TYPE saka_merge_requests_total counter\nsaka_merge_requests_total{by=\"autonomy\"} 2\nsaka_merge_requests_total{by=\"queue\"} 1\n";
        assert_eq!(counter(metrics, "saka_merge_requests_total", "autonomy"), 2);
        assert_eq!(counter(metrics, "saka_merge_requests_total", "operator"), 0);
    }
}
