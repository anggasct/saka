#![allow(clippy::unwrap_used, reason = "tests fail loudly on a broken setup")]

use saka_core::hook::{self, RawHook};
use saka_core::rejection::Rejection;
use saka_core::secret::SecretText;
use saka_core::vocab::HookRunState;
use saka_engine::{Cause, EngineError, HookRunFilter, Page};
use saka_testkit::{Layout, SHOP_REPO, Sim, SimConfig, repo_entry, saka_toml};

const STEPS: usize = 8_000;

fn raw(name: &str, mode: Option<&str>) -> RawHook {
    RawHook {
        name: name.into(),
        project: "shop".into(),
        on: "merged".into(),
        mode: mode.map(str::to_string),
        action: "webhook".into(),
        url: Some("https://ci.example.com/saka".into()),
        secret: Some("HOOK_SIGNING_KEY".into()),
        ..RawHook::default()
    }
}

fn toml(extra: &str) -> String {
    let mut text = saka_toml("shop", &[repo_entry("shop", SHOP_REPO, "")]);
    text.push_str(extra);
    text
}

fn sim(hooks: &[RawHook], extra: &str) -> Sim {
    let layout = Layout {
        saka_toml: toml(extra),
        ..Layout::default()
    };
    let sim = Sim::multi_repo(
        SimConfig {
            hooks: hook::parse_all(hooks).unwrap(),
            ..SimConfig::default()
        },
        &layout,
    );
    let cause = Cause {
        actor: "operator".into(),
        causation_id: None,
        correlation_id: uuid::Uuid::from_u128(42),
    };
    sim.engine()
        .set_secret(
            sim.project_id,
            "HOOK_SIGNING_KEY",
            &SecretText::new("signing-key-abcdef-123456".into()),
            &cause,
            None,
        )
        .unwrap();
    sim
}

fn ran(sim: &Sim) -> Vec<String> {
    let page = Page {
        after: None,
        limit: 200,
    };
    let mut names: Vec<String> = sim
        .engine()
        .hook_runs(
            &HookRunFilter {
                project: Some(sim.project_id),
                ..HookRunFilter::default()
            },
            &page,
        )
        .unwrap()
        .items
        .into_iter()
        .map(|r| r.hook)
        .collect();
    names.sort();
    names
}

fn settled(sim: &Sim) -> bool {
    let page = Page {
        after: None,
        limit: 200,
    };
    let runs = sim
        .engine()
        .hook_runs(
            &HookRunFilter {
                project: Some(sim.project_id),
                ..HookRunFilter::default()
            },
            &page,
        )
        .unwrap()
        .items;
    !runs.is_empty() && runs.iter().all(|r| r.state != HookRunState::Pending)
}

#[test]
fn a_repo_file_switches_off_an_after_hook_for_its_project_and_no_other() {
    let mut sim = sim(
        &[raw("announce", None), raw("audit", None)],
        "\n[hooks]\nskip = [\"announce\"]\n",
    );
    assert_eq!(sim.project().project.hook_skip, ["announce"]);
    sim.approve();
    assert!(sim.run_to_shipped(STEPS));
    assert!(sim.run_until(STEPS, settled));
    assert_eq!(ran(&sim), ["audit", "audit"], "one step, one merge each");
}

#[test]
fn a_skip_that_names_no_hook_or_a_gate_hook_is_refused_and_the_old_list_stays() {
    let sim = sim(
        &[raw("announce", None), raw("verify", Some("gate"))],
        "\n[hooks]\nskip = [\"announce\"]\n",
    );
    for bad in ["nothing-like-it", "verify"] {
        sim.world.put_doc(
            "docs/saka.toml",
            toml(&format!("\n[hooks]\nskip = [\"{bad}\"]\n")).as_bytes(),
        );
        let Err(EngineError::Rejected(Rejection::ProjectConfigInvalid { issues })) = sim.reload()
        else {
            panic!("{bad} was accepted");
        };
        assert_eq!(issues[0]["code"], "hook_skip_invalid", "{bad}");
        assert_eq!(issues[0]["field"], "hooks.skip[0]");
        assert_eq!(sim.project().project.hook_skip, ["announce"]);
    }
}

#[test]
fn an_empty_skip_list_turns_the_hook_back_on_at_the_next_reload() {
    let sim = sim(
        &[raw("announce", None)],
        "\n[hooks]\nskip = [\"announce\"]\n",
    );
    sim.world.put_doc("docs/saka.toml", toml("").as_bytes());
    sim.reload().unwrap();
    assert!(sim.project().project.hook_skip.is_empty());
}
