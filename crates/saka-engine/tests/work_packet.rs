#![allow(clippy::unwrap_used, reason = "tests fail loudly on a broken setup")]

use saka_core::model::ContextRef;
use saka_core::vocab::{ContextOrigin, Tool};
use saka_engine::packet::{
    EnvironmentFacts, PacketInput, PacketRepo, PacketSpec, PacketStep, render,
};

const GOLDEN: &str = include_str!("fixtures/work_packet.txt");

fn context(path: &str, origin: ContextOrigin, content: &str) -> (ContextRef, Vec<u8>) {
    (
        ContextRef {
            path: path.into(),
            origin,
            line: 7,
            bytes: content.len() as u64,
            content_hash: format!("sha256:{}", "0".repeat(64)),
        },
        content.as_bytes().to_vec(),
    )
}

fn full() -> PacketInput {
    PacketInput {
        step: PacketStep {
            task_key: "tasks-done/s1".into(),
            work: "Add a `done` command to the CLI store".into(),
            criteria: vec![
                ("AC-01".into(), "`tasks done <id>` marks the task done.".into()),
                ("AC-02".into(), "Unknown ids exit with code 2.".into()),
            ],
            commands: vec!["cd cli && go test ./...".into(), "make lint".into()],
        },
        spec: PacketSpec {
            revision_id: "0193f0c4-1b2a-7c3d-9e8f-001122334455".into(),
            path: "docs/specs/tasks-done.md".into(),
            source_commit: "9f1c".repeat(10),
            content: b"---\nsaka: spec\n---\n# Mark tasks as done\n\nSee [the architecture](../architecture.md).\n".to_vec(),
        },
        context: vec![
            context(
                "docs/architecture.md",
                ContextOrigin::Link,
                "# Architecture\n\nThe CLI keeps tasks in one JSON file.\n",
            ),
            context(
                "docs/conventions.md",
                ContextOrigin::Always,
                "---\ncontext: always\n---\n# Conventions\n\nExit codes: 0 ok, 2 usage.\n\n",
            ),
        ],
        environment: EnvironmentFacts {
            repo: Some(PacketRepo {
                name: "api".into(),
                full_name: "acme/shop".into(),
                base_branch: "main".into(),
            }),
            path_scope: vec!["services/api/**".into(), "docs/**".into()],
            ports: Some((41000, 3)),
            toolchains: vec![
                (Tool::Node, "26.13.0".into()),
                (Tool::Go, "1.26.2".into()),
            ],
            session_secrets: vec!["TEST_API_KEY".into()],
        },
        retry: None,
    }
}

#[test]
fn a_full_packet_matches_the_golden_text() {
    let packet = render(&full());
    assert_eq!(packet.text, GOLDEN);
    let documents: Vec<(u32, &str, Option<ContextOrigin>)> = packet
        .documents
        .iter()
        .map(|d| (d.index, d.path.as_str(), d.origin))
        .collect();
    assert_eq!(
        documents,
        vec![
            (1, "docs/specs/tasks-done.md", None),
            (2, "docs/architecture.md", Some(ContextOrigin::Link)),
            (3, "docs/conventions.md", Some(ContextOrigin::Always)),
        ]
    );
    assert_eq!(render(&full()), packet);
}

fn environment_lines(input: &PacketInput) -> Vec<String> {
    let text = render(input).text;
    let (_, rest) = text.split_once("\nEnvironment:\n").unwrap();
    let (env, _) = rest.split_once("\n\n").unwrap();
    env.lines().map(String::from).collect()
}

#[test]
fn the_environment_names_the_repository_and_the_paths_the_task_may_change() {
    let mut input = full();
    assert_eq!(
        environment_lines(&input)[..2],
        [
            "- Repository: api (acme/shop), base branch main",
            "- Paths you may change: services/api/**, docs/**",
        ]
    );
    input.environment.path_scope = vec!["**".into()];
    assert_eq!(environment_lines(&input)[1], "- Paths you may change: all");
    input.environment.path_scope = vec!["**".into(), "docs/**".into()];
    assert_eq!(environment_lines(&input)[1], "- Paths you may change: all");
    input.environment.path_scope = vec!["**".into(), "!secrets/**".into()];
    assert_eq!(
        environment_lines(&input)[1],
        "- Paths you may change: **, !secrets/**"
    );
    assert!(render(&input).text.contains(
        "Rules:\n- Work only inside this repository, and only in the paths you may change.\n"
    ));
}

#[test]
fn the_same_input_always_renders_the_same_bytes() {
    let a = render(&full());
    let mut reordered = full();
    reordered.environment.toolchains.reverse();
    assert_eq!(render(&reordered).text, a.text);
}
