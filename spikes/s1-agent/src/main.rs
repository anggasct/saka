//! Spike S1: can our own runtime drive the official agent CLIs reliably?
//!
//! Usage: s1-agent <claude|codex> <runs> [first-run]
//! Expects `.dev/s1/origin.git` (a local bare remote) under the current directory.

mod agent;
mod claude;
mod codex;
mod scenario;

use std::collections::BTreeMap;

use anyhow::{Result, bail};
use serde_json::{Value, json};

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let (cli, runs, first) = match args.as_slice() {
        [_, cli, runs] => (cli.as_str(), runs.parse::<usize>()?, 1),
        [_, cli, runs, first] => (
            cli.as_str(),
            runs.parse::<usize>()?,
            first.parse::<usize>()?,
        ),
        _ => bail!("usage: s1-agent <claude|codex> <runs> [first-run]"),
    };
    if cli != "claude" && cli != "codex" {
        bail!("unknown cli {cli}");
    }
    let root = std::env::current_dir()?.join(".dev/s1");
    let mut reports: Vec<Value> = vec![];
    for n in first..first + runs {
        let r = match scenario::run(cli, n, &root).await {
            Ok(r) => r,
            Err(e) => json!({ "cli": cli, "run": n, "error": format!("{e:#}") }),
        };
        let checks = &r["checks"];
        println!(
            "{cli} #{n}: {} | {:.0}s ${:.4} | {}",
            if r.get("error").is_some() {
                "ERROR"
            } else {
                "ran"
            },
            r["secs"].as_f64().unwrap_or(0.0),
            r["cost_usd"].as_f64().unwrap_or(0.0),
            checks
                .as_object()
                .map(|m| m
                    .iter()
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect::<Vec<_>>()
                    .join(" "))
                .unwrap_or_else(|| r["error"].to_string()),
        );
        reports.push(r);
        let path = root.join(format!("report-{cli}.json"));
        std::fs::write(
            &path,
            serde_json::to_string_pretty(&json!({ "runs": reports }))?,
        )?;
    }

    let mut pass: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for r in &reports {
        if let Some(checks) = r["checks"].as_object() {
            for (k, v) in checks {
                let e = pass.entry(k.clone()).or_default();
                e.1 += 1;
                e.0 += usize::from(v == true);
            }
        } else {
            pass.entry("errors".into()).or_default().0 += 1;
        }
    }
    println!("\n{cli}: {} runs", reports.len());
    for (k, (ok, total)) in pass {
        println!("  {k}: {ok}/{total}");
    }
    Ok(())
}
