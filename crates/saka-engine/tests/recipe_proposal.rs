#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests fail loudly on a broken setup"
)]

use proptest::prelude::*;
use saka_engine::recipe::{self, RECIPE_PATH, RecipeAction};
use saka_engine::versions::RepoFiles;
use serde_json::json;

fn present(paths: &[&str]) -> RepoFiles {
    let mut files = RepoFiles::new();
    for path in paths {
        files.add_present(path);
    }
    files
}

#[test]
fn each_lockfile_maps_to_its_setup_command_in_order() {
    for (paths, commands) in [
        (
            vec!["pnpm-lock.yaml", "package-lock.json"],
            vec!["pnpm install --frozen-lockfile"],
        ),
        (vec!["package-lock.json"], vec!["npm ci"]),
        (vec!["npm-shrinkwrap.json"], vec!["npm ci"]),
        (vec!["go.mod", "go.sum"], vec!["go mod download"]),
        (vec!["Cargo.lock"], vec!["cargo fetch --locked"]),
        (vec!["uv.lock", "poetry.lock"], vec!["uv sync --frozen"]),
        (
            vec![
                "uv.lock",
                "Cargo.lock",
                "go.sum",
                "go.mod",
                "pnpm-lock.yaml",
            ],
            vec![
                "pnpm install --frozen-lockfile",
                "go mod download",
                "cargo fetch --locked",
                "uv sync --frozen",
            ],
        ),
    ] {
        let proposal = recipe::propose(&present(&paths), 10);
        assert_eq!(proposal.action, RecipeAction::Create, "{paths:?}");
        assert_eq!(proposal.commands, commands, "{paths:?}");
        assert!(proposal.notes.is_empty(), "{paths:?}");
    }
}

#[test]
fn the_generated_recipe_is_byte_exact() {
    let proposal = recipe::propose(&present(&["pnpm-lock.yaml", "go.mod", "go.sum"]), 10);
    assert_eq!(
        proposal.content.as_deref(),
        Some(
            "# Read by Saka from the default branch; each task runs these commands in its own environment.\n[setup]\ncommands = [\"pnpm install --frozen-lockfile\", \"go mod download\"]\n"
        )
    );
}

#[test]
fn lockfiles_without_a_known_command_are_noted() {
    let proposal = recipe::propose(&present(&["yarn.lock", "bun.lockb", "poetry.lock"]), 10);
    assert_eq!(proposal.action, RecipeAction::None);
    assert_eq!(proposal.reason, Some("no_lockfile"));
    let notes: Vec<_> = proposal
        .notes
        .iter()
        .map(|n| (n.code, n.params.clone()))
        .collect();
    assert_eq!(
        notes,
        [
            ("setup_command_unknown", json!({ "lockfile": "yarn.lock" })),
            ("setup_command_unknown", json!({ "lockfile": "bun.lockb" })),
            (
                "setup_command_unknown",
                json!({ "lockfile": "poetry.lock" })
            ),
        ]
    );
    let mixed = recipe::propose(&present(&["bun.lock", "Cargo.lock"]), 10);
    assert_eq!(mixed.commands, ["cargo fetch --locked"]);
    assert_eq!(mixed.notes.len(), 1);
}

#[test]
fn go_needs_both_go_mod_and_go_sum() {
    let proposal = recipe::propose(&present(&["go.mod"]), 10);
    assert_eq!(proposal.action, RecipeAction::None);
    assert_eq!(proposal.reason, Some("no_lockfile"));
    assert!(proposal.content.is_none());
}

#[test]
fn an_existing_recipe_is_left_alone_and_its_issues_reported() {
    let mut files = present(&["pnpm-lock.yaml"]);
    files.add(
        RECIPE_PATH,
        b"ports = 99\n[setup]\ncommands = [\"make\"]\n".to_vec(),
    );
    let proposal = recipe::propose(&files, 10);
    assert_eq!(proposal.action, RecipeAction::None);
    assert_eq!(proposal.reason, Some("recipe_exists"));
    assert!(proposal.content.is_none() && proposal.commands.is_empty());
    let codes: Vec<_> = proposal.issues.iter().map(|i| i.code).collect();
    assert_eq!(codes, ["ports_out_of_range"]);

    let mut valid = present(&["pnpm-lock.yaml"]);
    valid.add(RECIPE_PATH, b"[setup]\ncommands = [\"make\"]\n".to_vec());
    let proposal = recipe::propose(&valid, 10);
    assert!(proposal.issues.is_empty());
    assert_eq!(proposal.action, RecipeAction::None);
}

proptest! {
    #[test]
    fn every_generated_recipe_parses_and_keeps_its_commands(
        lockfiles in prop::sample::subsequence(
            vec!["pnpm-lock.yaml", "package-lock.json", "npm-shrinkwrap.json", "yarn.lock", "bun.lock",
                 "bun.lockb", "go.mod", "go.sum", "Cargo.lock", "uv.lock", "poetry.lock"],
            0..=11,
        )
    ) {
        let proposal = recipe::propose(&present(&lockfiles), 10);
        match proposal.content {
            Some(text) => {
                prop_assert_eq!(proposal.action, RecipeAction::Create);
                let parsed = recipe::parse(&text, 10).unwrap();
                prop_assert_eq!(&parsed.setup, &proposal.commands);
                prop_assert!(parsed.toolchain.is_empty());
                let order = ["pnpm install", "npm ci", "go mod", "cargo", "uv"];
                let ranks: Vec<usize> = proposal.commands.iter()
                    .map(|c| order.iter().position(|o| c.starts_with(o)).unwrap())
                    .collect();
                prop_assert!(ranks.windows(2).all(|w| w[0] < w[1]));
            }
            None => {
                prop_assert_eq!(proposal.action, RecipeAction::None);
                prop_assert!(proposal.commands.is_empty());
                prop_assert_eq!(proposal.reason, Some("no_lockfile"));
            }
        }
    }
}
