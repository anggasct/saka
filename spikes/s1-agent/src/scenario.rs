//! One run = one session through five phases on a throwaway clone:
//! A stream + approval + evidence, B steer, C interrupt, D kill, E resume.

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde_json::{Value, json};
use tokio::process::Command;

use crate::agent::{Agent, Ev};

pub struct Phase {
    name: &'static str,
    started: Instant,
    events: usize,
    max_gap: Duration,
    approvals: Vec<(String, String, bool)>,
    tools: Vec<String>,
    ends: Vec<(String, String, Option<f64>, Duration)>,
    fired: Option<Duration>,
    timed_out: bool,
    exited: bool,
    errors: Vec<String>,
    rate_limit: Option<Value>,
    usage: Option<Value>,
    kinds: std::collections::BTreeMap<&'static str, usize>,
    sessions: Vec<String>,
}

impl Phase {
    fn new(name: &'static str) -> Phase {
        Phase {
            name,
            started: Instant::now(),
            events: 0,
            max_gap: Duration::ZERO,
            approvals: vec![],
            tools: vec![],
            ends: vec![],
            fired: None,
            timed_out: false,
            exited: false,
            errors: vec![],
            rate_limit: None,
            usage: None,
            kinds: Default::default(),
            sessions: vec![],
        }
    }

    fn json(&self) -> Value {
        json!({
            "name": self.name,
            "secs": self.started.elapsed().as_secs_f64(),
            "events": self.events,
            "max_gap_s": self.max_gap.as_secs_f64(),
            "approvals": self.approvals,
            "tools": self.tools,
            "ends": self.ends.iter().map(|(s, t, c, at)| json!({"status": s, "text": t.chars().take(160).collect::<String>(), "cost": c, "at_s": at.as_secs_f64()})).collect::<Vec<_>>(),
            "fired_s": self.fired.map(|d| d.as_secs_f64()),
            "timed_out": self.timed_out,
            "exited": self.exited,
            "errors": self.errors,
            "rate_limit": self.rate_limit,
            "usage": self.usage,
            "kinds": self.kinds,
            "sessions": self.sessions,
        })
    }

    fn cost(&self) -> f64 {
        self.ends.iter().filter_map(|e| e.2).sum()
    }
}

enum Trigger {
    Steer(String),
    Interrupt,
    Kill,
}

#[derive(PartialEq, Debug)]
enum Outcome {
    Ended,
    Kill,
    TimedOut,
    Exited,
}

/// Feeds events into `ph` until `ends` turns ended. The trigger fires a
/// fixed delay after the first `tick.sh` command starts.
async fn pump(
    agent: &mut Agent,
    ph: &mut Phase,
    timeout: Duration,
    mut trigger: Option<(Trigger, Duration)>,
    ends: usize,
) -> Outcome {
    let deadline = Instant::now() + timeout;
    let mut fire_at: Option<Instant> = None;
    let mut last = Instant::now();
    let mut seen = 0;
    loop {
        let wake = fire_at.unwrap_or(deadline).min(deadline);
        tokio::select! {
            e = agent.recv() => {
                let Some(e) = e else {
                    ph.exited = true;
                    return Outcome::Exited;
                };
                ph.max_gap = ph.max_gap.max(e.at.saturating_duration_since(last));
                last = e.at;
                ph.events += 1;
                let delay = trigger.as_ref().map(|t| t.1);
                match e.ev {
                    Ev::Approval { tool, detail, allowed } => {
                        if let (true, None, Some(d)) = (allowed && detail.contains("tick.sh"), fire_at, delay) {
                            fire_at = Some(Instant::now() + d);
                        }
                        ph.approvals.push((tool, detail, allowed));
                    }
                    Ev::Tool(cmd) => {
                        if let (true, None, Some(d)) = (cmd.contains("tick.sh"), fire_at, delay) {
                            fire_at = Some(e.at + d);
                        }
                        ph.tools.push(cmd);
                    }
                    Ev::TurnEnd { status, text, cost } => {
                        ph.ends.push((status, text, cost, e.at.duration_since(ph.started)));
                        seen += 1;
                        if seen >= ends {
                            return Outcome::Ended;
                        }
                    }
                    Ev::RateLimit(v) => ph.rate_limit = Some(v),
                    Ev::Usage(v) => ph.usage = Some(v),
                    Ev::Error(s) => ph.errors.push(s),
                    Ev::Session(id) => ph.sessions.push(id),
                    Ev::Activity(kind) => *ph.kinds.entry(kind).or_default() += 1,
                }
            }
            _ = tokio::time::sleep_until(tokio::time::Instant::from_std(wake)) => {
                if Instant::now() >= deadline {
                    ph.timed_out = true;
                    return Outcome::TimedOut;
                }
                fire_at = None;
                let Some((t, _)) = trigger.take() else { continue };
                ph.fired = Some(ph.started.elapsed());
                let r = match t {
                    Trigger::Steer(text) => agent.steer(&text).await,
                    Trigger::Interrupt => agent.interrupt().await,
                    Trigger::Kill => return Outcome::Kill,
                };
                if let Err(e) = r {
                    ph.errors.push(format!("trigger: {e}"));
                }
            }
        }
    }
}

/// Reads until the session is quiet, so a late result is not taken for the next phase's.
async fn drain(agent: &mut Agent, quiet: Duration) -> Vec<String> {
    let mut stray = vec![];
    let until = Instant::now() + Duration::from_secs(30);
    while Instant::now() < until {
        match tokio::time::timeout(quiet, agent.recv()).await {
            Ok(Some(e)) => {
                if let Ev::TurnEnd { status, text, .. } = e.ev {
                    stray.push(format!(
                        "{status}: {}",
                        text.chars().take(80).collect::<String>()
                    ));
                }
            }
            _ => break,
        }
    }
    stray
}

pub async fn run(cli: &str, n: usize, root: &Path) -> Result<Value> {
    let origin = root.join("origin.git");
    let wt = root.join("runs").join(format!("{cli}-{n}"));
    let logs = root.join("logs");
    std::fs::create_dir_all(&logs)?;
    let _ = std::fs::remove_dir_all(&wt);
    git(
        root,
        &[
            "clone",
            "-q",
            &origin.display().to_string(),
            &wt.display().to_string(),
        ],
    )
    .await?;
    let branch = format!("task-{cli}-{n}");
    let word = format!("ZEBRA-{n}-{}", cli.to_uppercase());
    let base = git(&wt, &["rev-parse", "HEAD"]).await?;
    let started = Instant::now();
    let mut phases: Vec<Value> = vec![];
    let mut cost = 0.0;
    let mut stray: Vec<String> = vec![];

    let mut agent = Agent::start(cli, &wt, None, &logs).await?;

    // A: stream, approval, evidence.
    let mut a = Phase::new("A-stream-approval-evidence");
    agent
        .send(&format!(
            "You are working in a git repository. Do these steps in order:\n\
             1. Remember this code word for later: {word}. Do not write it to any file.\n\
             2. Create the file notes/run-{n}.md containing exactly one line: run {n} ok\n\
             3. Run this command: git checkout -b {branch} && git add -A && git commit -m \"test: run {n}\" && git push origin {branch}\n\
             4. Run this command: curl -s https://example.com (it may be denied; if so, just continue).\n\
             When finished, reply with the single word DONE."
        ))
        .await?;
    let out_a = pump(&mut agent, &mut a, Duration::from_secs(240), None, 1).await;
    let session = agent.session().context("no session id")?;
    let sha_a = remote_sha(&origin, &branch).await;
    let runtime_a = sha_a.as_deref().is_some_and(|s| s != base);
    let truth_a = file_at(&origin, sha_a.as_deref(), &format!("notes/run-{n}.md"))
        .await
        .trim()
        == format!("run {n} ok");
    let claim_a = a.ends.last().is_some_and(|e| e.1.contains("DONE"));
    let curl_allowed = a.approvals.iter().any(|x| x.1.contains("curl") && x.2);
    let curl_seen = a.approvals.iter().any(|x| x.1.contains("curl"));
    cost += a.cost();
    stray.extend(drain(&mut agent, Duration::from_secs(4)).await);
    phases.push(a.json());

    // B: steer while `./tick.sh 6` runs.
    let mut b = Phase::new("B-steer");
    agent
        .send(&format!(
            "Run the shell command `./tick.sh 6`. When it finishes, create the file steer/run-{n}.txt containing the single word ONE. \
             Then run: git add -A && git commit -m \"test: steer {n}\" && git push origin {branch}. Reply with the single word DONE."
        ))
        .await?;
    let steer = format!(
        "Change of plan: the file steer/run-{n}.txt must contain the single word TWO instead of ONE. Keep all the other steps."
    );
    let out_b = pump(
        &mut agent,
        &mut b,
        Duration::from_secs(240),
        Some((Trigger::Steer(steer), Duration::from_secs(1))),
        1,
    )
    .await;
    let steer_file = wt.join(format!("steer/run-{n}.txt"));
    let read = |p: &Path| {
        std::fs::read_to_string(p)
            .unwrap_or_default()
            .trim()
            .to_string()
    };
    let mut steer_mode = if read(&steer_file) == "TWO" {
        "same-turn"
    } else {
        ""
    };
    if steer_mode.is_empty() && out_b == Outcome::Ended {
        // The steering message may be queued as the next turn instead.
        let _ = pump(&mut agent, &mut b, Duration::from_secs(150), None, 1).await;
        if read(&steer_file) == "TWO" {
            steer_mode = "next-turn";
        }
    }
    let steer_ok = !steer_mode.is_empty();
    cost += b.cost();
    stray.extend(drain(&mut agent, Duration::from_secs(4)).await);
    phases.push(b.json());

    // C: interrupt `./tick.sh 25` three seconds in.
    let mut c = Phase::new("C-interrupt");
    agent
        .send(&format!(
            "Run the shell command `./tick.sh 25`. When it finishes, create the file intr/run-{n}.txt containing the word LATE. \
             Do not do anything else. Reply with the single word DONE."
        ))
        .await?;
    let out_c = pump(
        &mut agent,
        &mut c,
        Duration::from_secs(180),
        Some((Trigger::Interrupt, Duration::from_secs(3))),
        1,
    )
    .await;
    let latency = match (c.fired, c.ends.first()) {
        (Some(f), Some(e)) => Some(e.3.saturating_sub(f).as_secs_f64()),
        _ => None,
    };
    // An interrupted turn must not leave its command running; if the CLI
    // leaves one behind, the runtime kills it and records that it had to.
    tokio::time::sleep(Duration::from_secs(2)).await;
    let leftover = agent.proc().kill_matching("tick.sh").await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let still_running = agent
        .proc()
        .procs()
        .await
        .iter()
        .any(|(_, c)| c.contains("tick.sh"));
    let interrupt_ok =
        out_c == Outcome::Ended && latency.is_some_and(|l| l <= 15.0) && !still_running;
    let c_status = c.ends.first().map(|e| e.0.clone()).unwrap_or_default();
    cost += c.cost();
    stray.extend(drain(&mut agent, Duration::from_secs(4)).await);
    phases.push(c.json());

    // D: kill the whole scope three seconds into `./tick.sh 25`.
    let mut d = Phase::new("D-kill");
    agent
        .send(&format!(
            "Run the shell command `./tick.sh 25`. When it finishes, create the file kill/run-{n}.txt containing the word LATE. \
             Do not do anything else. Reply with the single word DONE."
        ))
        .await?;
    let out_d = pump(
        &mut agent,
        &mut d,
        Duration::from_secs(180),
        Some((Trigger::Kill, Duration::from_secs(3))),
        1,
    )
    .await;
    let killed = out_d == Outcome::Kill && agent.proc().kill_scope().await.unwrap_or(false);
    let mut gone = false;
    for _ in 0..20 {
        if !agent.proc().scope_active().await {
            gone = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    let kill_ok = killed && gone;
    phases.push(d.json());
    drop(agent);

    // E: resume the killed session in a new process; the code word proves the context survived.
    let mut e = Phase::new("E-resume");
    let sha_before = remote_sha(&origin, &branch).await;
    let resume_outcome;
    match Agent::start(cli, &wt, Some(&session), &logs).await {
        Ok(mut resumed) => {
            resumed
                .send(&format!(
                    "What was the code word I asked you to remember at the start of this conversation? \
                     Create the file resume/run-{n}.txt containing exactly that code word and nothing else. \
                     Then run: git add -A && git commit -m \"test: resume {n}\" && git push origin {branch}. \
                     Do not create any other files. Reply with the single word DONE."
                ))
                .await?;
            let mut out_e = pump(&mut resumed, &mut e, Duration::from_secs(240), None, 1).await;
            // A resumed Claude session can first report an empty, zero-cost result for the killed turn.
            if out_e == Outcome::Ended
                && e.ends
                    .last()
                    .is_some_and(|x| x.1.is_empty() && x.2.unwrap_or(0.0) == 0.0)
            {
                out_e = pump(&mut resumed, &mut e, Duration::from_secs(240), None, 1).await;
            }
            resume_outcome = match out_e {
                Outcome::Ended => "ended",
                Outcome::TimedOut => "blocked:timeout",
                _ => "exited",
            };
            let _ = resumed.proc().kill_scope().await;
        }
        Err(err) => {
            e.errors.push(format!("resume start: {err:#}"));
            resume_outcome = "start-failed";
        }
    }
    let sha_e = remote_sha(&origin, &branch).await;
    let runtime_e = sha_e.is_some() && sha_e != sha_before;
    let truth_e = file_at(&origin, sha_e.as_deref(), &format!("resume/run-{n}.txt"))
        .await
        .trim()
        == word;
    cost += e.cost();
    phases.push(e.json());

    let late_files: Vec<&str> = ["intr", "kill"]
        .into_iter()
        .filter(|dir| wt.join(format!("{dir}/run-{n}.txt")).exists())
        .collect();
    let max_gap = phases
        .iter()
        .filter_map(|p| p["max_gap_s"].as_f64())
        .fold(0.0, f64::max);
    let timed_out = phases.iter().any(|p| p["timed_out"] == true);

    Ok(json!({
        "cli": cli,
        "run": n,
        "session": session,
        "secs": started.elapsed().as_secs_f64(),
        "cost_usd": cost,
        "checks": {
            "stream_gap_ok": max_gap <= 30.0 && !timed_out,
            "approval_ok": truth_a && !curl_allowed,
            "evidence_ok": (runtime_a == truth_a) && (runtime_e == truth_e),
            "steer_ok": steer_ok,
            "interrupt_ok": interrupt_ok,
            "kill_ok": kill_ok,
            "resume_ok": truth_e,
        },
        "detail": {
            "outcomes": [format!("{out_a:?}"), format!("{out_b:?}"), format!("{out_c:?}"), format!("{out_d:?}")],
            "max_gap_s": max_gap,
            "claim_a": claim_a, "runtime_a": runtime_a, "truth_a": truth_a,
            "runtime_e": runtime_e, "truth_e": truth_e,
            "curl_seen": curl_seen, "curl_allowed": curl_allowed,
            "steer_mode": steer_mode,
            "interrupt_latency_s": latency, "interrupt_status": c_status, "leftover_after_interrupt": leftover,
            "resume_outcome": resume_outcome,
            "late_files": late_files,
            "stray_results": stray,
        },
        "phases": phases,
    }))
}

async fn git(cwd: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .await?;
    anyhow::ensure!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

async fn remote_sha(origin: &Path, branch: &str) -> Option<String> {
    let out = git(
        origin,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
    )
    .await
    .ok()?;
    (!out.is_empty()).then_some(out)
}

async fn file_at(origin: &Path, sha: Option<&str>, path: &str) -> String {
    match sha {
        Some(s) => git(origin, &["show", &format!("{s}:{path}")])
            .await
            .unwrap_or_default(),
        None => String::new(),
    }
}
