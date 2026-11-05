#![allow(clippy::unwrap_used, reason = "tests fail loudly on a broken setup")]

use saka_core::hook::{self, HookRun, RawHook};
use saka_core::secret::SecretText;
use saka_core::vocab::HookRunState;
use saka_engine::{Cause, HookRunFilter, Page};
use saka_ports::PortError;
use saka_testkit::{CiScript, CommandScript, HostScript, Sim, SimConfig, WebhookScript};
use serde_json::json;

const STEPS: usize = 8_000;
const SIGNING_KEY: &str = "signing-key-abcdef-123456";

fn raw(name: &str, on: &str, action: &str) -> RawHook {
    RawHook {
        name: name.into(),
        project: "tasks".into(),
        on: on.into(),
        action: action.into(),
        url: (action == "webhook").then(|| "https://ci.example.com/saka?token=1".into()),
        secret: (action == "webhook").then(|| "HOOK_SIGNING_KEY".into()),
        channel: (action == "notify").then(|| "ops".into()),
        ..RawHook::default()
    }
}

fn sim(hooks: Vec<RawHook>) -> Sim {
    let sim = Sim::new(SimConfig {
        hooks: hook::parse_all(&hooks).unwrap(),
        ..SimConfig::default()
    });
    set_secret(&sim, "HOOK_SIGNING_KEY", SIGNING_KEY);
    sim
}

fn set_secret(sim: &Sim, name: &str, value: &str) {
    let cause = Cause {
        actor: "operator".into(),
        causation_id: None,
        correlation_id: uuid::Uuid::from_u128(42),
    };
    sim.engine()
        .set_secret(
            sim.project_id,
            name,
            &SecretText::new(value.into()),
            &cause,
            None,
        )
        .unwrap();
}

fn runs(sim: &Sim) -> Vec<HookRun> {
    let mut all: Vec<HookRun> = sim
        .engine()
        .hook_runs(
            &HookRunFilter {
                project: Some(sim.project_id),
                ..HookRunFilter::default()
            },
            &Page {
                after: None,
                limit: 200,
            },
        )
        .unwrap()
        .items;
    all.sort_by_key(|r| (r.queued_at, r.hook.clone(), r.id.0));
    all
}

fn settled(sim: &Sim) -> bool {
    let all = runs(sim);
    !all.is_empty() && all.iter().all(|r| r.state != HookRunState::Pending)
}

fn deliver(sim: &mut Sim) {
    sim.approve();
    assert!(sim.run_to_shipped(STEPS));
    assert!(sim.run_until(STEPS, settled));
}

#[test]
fn a_merge_queues_one_signed_webhook_run_in_the_same_transaction() {
    let mut sim = sim(vec![raw("announce", "merged", "webhook")]);
    deliver(&mut sim);

    let all = runs(&sim);
    assert_eq!(all.len(), 2, "one run per merged step");
    let run = &all[0];
    assert_eq!(run.state, HookRunState::Succeeded);
    assert_eq!(run.attempts, 1);
    assert_eq!(run.trigger.as_str(), "merged");
    assert_eq!(run.context.repo.as_ref().unwrap().name, "app");
    assert_eq!(
        run.context.task.as_ref().unwrap().key,
        sim.task(1).item.task_key
    );
    assert_eq!(run.result.as_ref().unwrap()["http_status"], 200);

    let posts = sim.hooks.webhooks();
    assert_eq!(posts.len(), 2);
    assert_eq!(posts[0].url, "https://ci.example.com/saka?token=1");
    assert_eq!(posts[0].event, "merged");
    assert_eq!(posts[0].delivery_id, run.id.to_string());
    assert_eq!(posts[0].secret, SIGNING_KEY);
    let body: serde_json::Value = serde_json::from_str(&posts[0].body).unwrap();
    assert_eq!(body["id"], json!(run.id));
    assert_eq!(body["hook"], "announce");
    assert_eq!(body["event"], "merged");
    assert_eq!(body["project"]["name"], "tasks");
    assert_eq!(body["sha"], json!(run.sha));

    let merged = sim.events_of("scm.change_request.merged");
    let queued = sim.events_of("delivery.hook_run.queued");
    assert_eq!(queued.len(), 2);
    assert_eq!(
        queued[0].seq,
        merged[0].seq + 1,
        "queued in the merge transaction"
    );
    assert_eq!(queued[0].correlation_id, merged[0].correlation_id);
    assert_eq!(sim.events_of("delivery.hook_run.finished").len(), 2);
}

#[test]
fn only_matching_hooks_run() {
    let mut other_project = raw("elsewhere", "merged", "webhook");
    other_project.project = "someone-else".into();
    let mut other_repo = raw("web-only", "merged", "webhook");
    other_repo.repos = vec!["web".into()];
    let mut other_branch = raw("release-only", "merged", "webhook");
    other_branch.branches = vec!["release/*".into()];
    let mut matching = raw("main-only", "merged", "webhook");
    matching.branches = vec!["main".into()];
    matching.repos = vec!["app".into()];
    let mut sim = sim(vec![
        other_project,
        other_repo,
        other_branch,
        matching,
        raw("done", "task_done", "webhook"),
        raw("opened", "change_opened", "webhook"),
        raw("never", "blocked", "webhook"),
    ]);
    deliver(&mut sim);
    let mut names: Vec<String> = runs(&sim).into_iter().map(|r| r.hook).collect();
    names.sort();
    assert_eq!(
        names,
        ["done", "done", "main-only", "main-only", "opened", "opened"],
        "two steps, three matching hooks"
    );
}

#[test]
fn a_shipped_spec_triggers_hooks_from_the_reconciler() {
    let mut sim = sim(vec![raw("shipped", "spec_shipped", "webhook")]);
    deliver(&mut sim);
    let all = runs(&sim);
    assert_eq!(all.len(), 1);
    assert!(all[0].work_item_id.is_none());
    assert!(all[0].spec_id.is_some());
    assert_eq!(all[0].context.spec.as_ref().unwrap().path, sim.spec().path);
}

#[test]
fn a_transient_failure_is_retried_and_a_permanent_one_is_not() {
    let mut sim = sim(vec![raw("announce", "merged", "webhook")]);
    sim.hooks.script_webhook(WebhookScript::Status(503));
    deliver(&mut sim);
    let all = runs(&sim);
    assert_eq!(all[0].state, HookRunState::Succeeded);
    assert_eq!(all[0].attempts, 2);
    assert_eq!(all[1].attempts, 1);
    assert_eq!(sim.hooks.webhooks().len(), 3);

    let mut sim = self::sim(vec![raw("announce", "merged", "webhook")]);
    sim.hooks.script_webhook(WebhookScript::Status(404));
    deliver(&mut sim);
    let all = runs(&sim);
    assert_eq!(all[0].state, HookRunState::Failed);
    assert_eq!(all[0].error.as_ref().unwrap().code, "hook_webhook_rejected");
    assert_eq!(all[0].attempts, 1);
    assert_eq!(all[1].state, HookRunState::Succeeded);
    assert_eq!(sim.hooks.webhooks().len(), 2);
}

#[test]
fn a_receiver_that_stays_down_ends_as_a_visible_failure() {
    let mut sim = sim(vec![raw("announce", "merged", "webhook")]);
    for _ in 0..6 {
        sim.hooks.script_webhook(WebhookScript::Unavailable);
    }
    deliver(&mut sim);
    for run in runs(&sim) {
        assert_eq!(run.state, HookRunState::Failed);
        assert_eq!(run.error.as_ref().unwrap().code, "retries_exhausted");
        assert_eq!(run.attempts, 3);
    }
    let metrics = sim.engine().metrics().unwrap();
    assert!(
        metrics
            .hook_runs
            .contains(&("webhook".into(), "after".into(), "failed".into(), 2))
    );
}

#[test]
fn a_missing_signing_secret_fails_without_a_request() {
    let mut hook = raw("announce", "merged", "webhook");
    hook.secret = Some("NOT_SET".into());
    let mut sim = sim(vec![hook]);
    deliver(&mut sim);
    let all = runs(&sim);
    assert!(all.iter().all(|r| r.state == HookRunState::Failed));
    let run = &all[0];
    assert_eq!(run.error.as_ref().unwrap().code, "hook_secret_missing");
    assert_eq!(run.error.as_ref().unwrap().params["name"], "NOT_SET");
    assert!(sim.hooks.webhooks().is_empty());
}

#[test]
fn a_notify_hook_hands_a_message_code_and_parameters_to_the_notifier() {
    let mut sim = sim(vec![raw("tell-ops", "task_done", "notify")]);
    deliver(&mut sim);
    let run = runs(&sim).remove(0);
    assert_eq!(run.state, HookRunState::Succeeded);
    let sent = sim.hooks.notifications();
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[0].channel, "ops");
    assert_eq!(sent[0].code, "hook.event");
    assert_eq!(sent[0].params["event"], "task_done");
    assert_eq!(sent[0].dedupe_key, run.id.to_string());
}

#[test]
fn an_unconfigured_channel_is_a_visible_failure() {
    let mut sim = sim(vec![raw("tell-ops", "task_done", "notify")]);
    sim.hooks.script_notify(PortError::permanent(
        "notify_channel_unconfigured",
        json!({ "channel": "ops" }),
    ));
    deliver(&mut sim);
    let all = runs(&sim);
    assert_eq!(all[0].state, HookRunState::Failed);
    assert_eq!(
        all[0].error.as_ref().unwrap().code,
        "notify_channel_unconfigured"
    );
    assert_eq!(all[1].state, HookRunState::Succeeded);
}

#[test]
fn a_restart_does_not_queue_or_run_anything_twice() {
    let mut sim = sim(vec![raw("announce", "merged", "webhook")]);
    deliver(&mut sim);
    sim.restart();
    for _ in 0..50 {
        sim.step();
    }
    assert_eq!(runs(&sim).len(), 2);
    assert_eq!(sim.hooks.webhooks().len(), 2);
}

#[test]
fn a_rerun_makes_a_new_run_from_the_current_definition() {
    let mut sim = sim(vec![raw("announce", "merged", "webhook")]);
    sim.hooks.script_webhook(WebhookScript::Status(404));
    deliver(&mut sim);
    let first = runs(&sim).remove(0);
    assert_eq!(first.state, HookRunState::Failed);

    let engine = sim.engine();
    let cause = engine.cause("human:operator:local");
    let again = engine
        .rerun_hook(first.id, "receiver fixed", &cause, None)
        .unwrap();
    assert_eq!(again.rerun_of, Some(first.id));
    assert_eq!(again.state, HookRunState::Pending);
    assert_eq!(again.context, first.context);
    assert!(
        engine
            .rerun_hook(again.id, "too early", &cause, None)
            .is_err()
    );
    assert!(sim.run_until(STEPS, |s| {
        runs(s).iter().all(|r| r.state != HookRunState::Pending)
    }));
    let all = runs(&sim);
    assert_eq!(all.len(), 3);
    assert_eq!(
        all.iter()
            .filter(|r| r.state == HookRunState::Failed)
            .count(),
        1
    );
}

#[test]
fn no_secret_value_reaches_the_event_log() {
    let mut sim = sim(vec![raw("announce", "merged", "webhook")]);
    deliver(&mut sim);
    let events = sim.engine().events_after(0, 10_000).unwrap();
    for e in events {
        assert!(!e.payload.to_string().contains(SIGNING_KEY), "{}", e.kind);
    }
}

#[test]
fn without_hooks_the_loop_queues_nothing() {
    let mut sim = Sim::new(SimConfig::default());
    sim.approve();
    assert!(sim.run_to_shipped(STEPS));
    assert!(runs(&sim).is_empty());
    assert!(sim.events_of("delivery.hook_run.queued").is_empty());
}

#[test]
fn a_hook_removed_from_the_config_cannot_be_run_again() {
    let mut sim = sim(vec![raw("announce", "merged", "webhook")]);
    sim.hooks.script_webhook(WebhookScript::Status(404));
    deliver(&mut sim);
    let failed = runs(&sim).remove(0);
    sim.cfg.hooks.clear();
    sim.restart();
    let engine = sim.engine();
    let cause = engine.cause("human:operator:local");
    let err = engine
        .rerun_hook(failed.id, "retry", &cause, None)
        .unwrap_err();
    assert!(
        matches!(
            err,
            saka_engine::EngineError::Rejected(saka_core::rejection::Rejection::HookNotConfigured { ref hook }) if hook == "announce"
        ),
        "{err:?}"
    );
}

#[test]
fn a_pending_run_whose_hook_is_gone_fails_visibly() {
    let mut sim = sim(vec![raw("announce", "merged", "webhook")]);
    sim.approve();
    assert!(sim.run_until(STEPS, |s| {
        !s.events_of("delivery.hook_run.queued").is_empty()
    }));
    sim.cfg.hooks.clear();
    sim.restart();
    assert!(sim.run_until(STEPS, settled));
    let run = runs(&sim).remove(0);
    assert_eq!(run.state, HookRunState::Failed);
    assert_eq!(run.error.as_ref().unwrap().code, "hook_not_configured");
}

fn command_hook(name: &str, on: &str, action: &str) -> RawHook {
    RawHook {
        name: name.into(),
        project: "tasks".into(),
        on: on.into(),
        action: action.into(),
        command: Some("./deploy.sh staging".into()),
        ..RawHook::default()
    }
}

fn hook_runs_of(sim: &Sim, action: &str) -> Vec<saka_testkit::RecipeRun> {
    sim.world
        .recipe_runs()
        .into_iter()
        .filter(|r| r.phase == action)
        .collect()
}

#[test]
fn a_run_hook_executes_in_a_throwaway_environment_at_the_merge_commit() {
    let mut hook = command_hook("deploy", "merged", "run");
    hook.secrets = Some(vec!["DEPLOY_TOKEN".into()]);
    let mut sim = sim(vec![hook]);
    set_secret(&sim, "DEPLOY_TOKEN", "deploy-token-123456");
    set_secret(&sim, "UNRELATED", "unrelated-secret-1");
    deliver(&mut sim);

    let all = runs(&sim);
    assert!(
        all.iter().all(|r| r.state == HookRunState::Succeeded),
        "{all:?}"
    );
    let first = &all[0];
    assert_eq!(first.result.as_ref().unwrap()["exit_code"], 0);

    let ran = hook_runs_of(&sim, "hook");
    assert_eq!(ran.len(), 2);
    assert_eq!(ran[0].command, "./deploy.sh staging");
    assert_eq!(
        ran[0].environment_id.0, first.id.0,
        "the environment is the run's own"
    );
    assert_eq!(ran[0].secrets.len(), 1);
    assert_eq!(ran[0].secrets["DEPLOY_TOKEN"], "deploy-token-123456");
    assert_eq!(ran[0].redacted, ["DEPLOY_TOKEN"]);
    assert_eq!(ran[0].vars["SAKA_EVENT"], "merged");
    assert_eq!(ran[0].vars["SAKA_PROJECT"], "tasks");
    assert_eq!(ran[0].vars["SAKA_REPO"], "app");
    assert_eq!(ran[0].vars["SAKA_SHA"], first.sha.clone().unwrap());
    assert_eq!(ran[0].vars["SAKA_HOOK_RUN"], first.id.to_string());
    assert!(ran[0].vars.contains_key("SAKA_PR"));
    assert!(ran[0].vars.contains_key("SAKA_TASK_KEY"));
    assert!(
        !ran[0]
            .vars
            .values()
            .any(|v| v.contains("secret") || v.contains("token"))
    );

    let checkout = sim
        .world
        .checkouts()
        .into_iter()
        .find(|(env, _)| env.0 == first.id.0)
        .unwrap();
    assert_eq!(Some(checkout.1), first.sha);
    let stats = sim.world.stats();
    assert_eq!(
        stats.prepares, stats.teardowns,
        "every environment is torn down"
    );
    for e in sim.engine().events_after(0, 10_000).unwrap() {
        assert!(
            !e.payload.to_string().contains("deploy-token-123456"),
            "{}",
            e.kind
        );
    }
}

#[test]
fn a_failing_or_slow_command_fails_the_run_once_and_still_tears_the_environment_down() {
    let mut sim = sim(vec![command_hook("deploy", "merged", "run")]);
    sim.world
        .script_command("./deploy.sh staging", CommandScript::Exit(3));
    sim.world
        .script_command("./deploy.sh staging", CommandScript::Timeout);
    deliver(&mut sim);
    let all = runs(&sim);
    let codes: Vec<_> = all
        .iter()
        .map(|r| r.error.as_ref().map(|e| e.code.clone()))
        .collect();
    assert!(
        codes.contains(&Some("hook_command_failed".into())),
        "{codes:?}"
    );
    assert!(codes.contains(&Some("hook_timed_out".into())), "{codes:?}");
    assert!(
        all.iter().all(|r| r.attempts == 1),
        "a failed command is never repeated"
    );
    let failed = all
        .iter()
        .find(|r| r.error.as_ref().unwrap().code == "hook_command_failed")
        .unwrap();
    assert_eq!(failed.error.as_ref().unwrap().params["exit_code"], 3);
    assert_eq!(hook_runs_of(&sim, "hook").len(), 2);
    let stats = sim.world.stats();
    assert_eq!(stats.prepares, stats.teardowns);
}

#[test]
fn a_supervisor_that_keeps_crashing_ends_as_retries_exhausted_and_leaves_no_environment() {
    let mut sim = sim(vec![command_hook("deploy", "task_done", "run")]);
    for _ in 0..6 {
        sim.world
            .script_command("./deploy.sh staging", CommandScript::Crash);
    }
    deliver(&mut sim);
    for run in runs(&sim) {
        assert_eq!(run.state, HookRunState::Failed);
        assert_eq!(run.error.as_ref().unwrap().code, "retries_exhausted");
        assert_eq!(run.attempts, 3);
    }
    let stats = sim.world.stats();
    assert_eq!(stats.prepares, stats.teardowns);
}

#[test]
fn a_missing_run_secret_fails_before_any_environment_exists() {
    let mut hook = command_hook("deploy", "merged", "run");
    hook.secrets = Some(vec!["NOT_SET".into()]);
    let mut sim = sim(vec![hook]);
    let before = sim.world.stats().prepares;
    deliver(&mut sim);
    let all = runs(&sim);
    assert!(
        all.iter()
            .all(|r| r.error.as_ref().unwrap().code == "hook_secret_missing")
    );
    assert!(hook_runs_of(&sim, "hook").is_empty());
    assert!(sim.world.stats().prepares >= before);
    assert!(
        !sim.world
            .prepared()
            .iter()
            .any(|p| p.environment_id.0 == all[0].id.0)
    );
}

#[test]
fn a_spec_shipped_run_uses_the_tip_of_the_named_repos_base_branch() {
    let mut hook = command_hook("release", "spec_shipped", "run");
    hook.repos = vec!["app".into()];
    let mut sim = sim(vec![hook]);
    deliver(&mut sim);
    let run = runs(&sim).remove(0);
    assert_eq!(run.state, HookRunState::Succeeded, "{:?}", run.error);
    assert!(run.work_item_id.is_none());
    let checkout = sim
        .world
        .checkouts()
        .into_iter()
        .find(|(env, _)| env.0 == run.id.0)
        .unwrap();
    assert_eq!(checkout.1, sim.world.base_head());
    let vars = &hook_runs_of(&sim, "hook")[0].vars;
    assert_eq!(vars["SAKA_SPEC"], sim.spec().path);
    assert!(!vars.contains_key("SAKA_SHA"));
}

#[test]
fn a_spec_shipped_run_naming_an_unbound_repo_fails_visibly() {
    let mut hook = command_hook("release", "spec_shipped", "run");
    hook.repos = vec!["nope".into()];
    let mut sim = sim(vec![hook]);
    deliver(&mut sim);
    let run = runs(&sim).remove(0);
    assert_eq!(run.error.as_ref().unwrap().code, "hook_commit_unavailable");
}

#[test]
fn a_host_hook_gets_the_event_variables_and_no_secrets() {
    let mut hook = command_hook("notify-host", "merged", "run_host");
    hook.cwd = Some("/srv/deploy".into());
    let mut sim = sim(vec![hook]);
    set_secret(&sim, "DEPLOY_TOKEN", "deploy-token-123456");
    sim.hooks.host_output("deployed");
    deliver(&mut sim);
    let all = runs(&sim);
    assert!(all.iter().all(|r| r.state == HookRunState::Succeeded));
    assert_eq!(all[0].result.as_ref().unwrap()["output_tail"], "deployed");
    let calls = sim.hooks.host_calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].command, "./deploy.sh staging");
    assert_eq!(calls[0].cwd.as_deref(), Some("/srv/deploy"));
    assert_eq!(
        calls[0].scratch_key,
        format!("{}/notify-host", sim.project_id)
    );
    assert_eq!(calls[0].env["SAKA_EVENT"], "merged");
    assert_eq!(calls[0].env["SAKA_HOOK"], "notify-host");
    assert!(calls[0].env.keys().all(|k| k.starts_with("SAKA_")));
    assert!(!format!("{:?}", calls[0]).contains("deploy-token-123456"));
    assert!(
        hook_runs_of(&sim, "hook").is_empty(),
        "no sandbox environment is made"
    );
}

#[test]
fn a_host_command_that_exits_with_an_error_or_times_out_is_not_repeated() {
    let mut sim = sim(vec![command_hook("host", "task_done", "run_host")]);
    sim.hooks.script_host(HostScript::Exit(9));
    sim.hooks.script_host(HostScript::Timeout);
    deliver(&mut sim);
    let all = runs(&sim);
    let mut codes: Vec<String> = all
        .iter()
        .map(|r| r.error.as_ref().unwrap().code.clone())
        .collect();
    codes.sort();
    assert_eq!(codes, ["hook_command_failed", "hook_timed_out"]);
    assert!(all.iter().all(|r| r.attempts == 1));
    assert_eq!(sim.hooks.host_calls().len(), 2);
}

#[test]
fn a_host_runner_that_cannot_start_is_retried() {
    let mut sim = sim(vec![command_hook("host", "task_done", "run_host")]);
    sim.hooks.script_host(HostScript::Crash);
    deliver(&mut sim);
    let all = runs(&sim);
    assert!(all.iter().all(|r| r.state == HookRunState::Succeeded));
    assert_eq!(all[0].attempts, 2);
}

#[test]
fn a_blocked_task_notifies_with_its_blocker_and_the_action_a_button_could_take() {
    let mut cfg = SimConfig {
        hooks: hook::parse_all(&[raw("tell-ops", "blocked", "notify")]).unwrap(),
        ci: CiScript::Pass,
        ..SimConfig::default()
    };
    cfg.operator.merge = false;
    cfg.operator.retry = vec![];
    let mut sim = Sim::new(cfg);
    for _ in 0..9 {
        sim.world
            .script_command("cd cli && go test ./...", CommandScript::Crash);
    }
    sim.approve();
    assert!(sim.run_until(STEPS, |s| settled(s) && !s.hooks.notifications().is_empty()));

    let task = sim.task(1);
    let blocker = task.open_blocker().cloned().unwrap();
    let sent = sim.hooks.notifications();
    assert_eq!(sent[0].channel, "ops");
    let params = &sent[0].params;
    assert_eq!(params["event"], "blocked");
    assert_eq!(params["task"]["id"], task.item.id.0.to_string());
    assert_eq!(params["blocker"]["id"], blocker.id.0.to_string());
    assert_eq!(params["blocker"]["code"], "ci_red");
    assert_eq!(params["blocker"]["action"], "retry");
    let run = runs(&sim).remove(0);
    assert_eq!(run.context.blocker.unwrap().id, blocker.id);
}
