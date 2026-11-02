#![allow(clippy::unwrap_used, reason = "tests fail loudly on a broken setup")]

use saka_core::ids::SpecRevisionId;
use saka_core::model::ContextRef;
use saka_core::rejection::Rejection;
use saka_core::vocab::{ContextOrigin, WorkItemState};
use saka_engine::EngineError;
use saka_store::read;
use saka_testkit::{APPROVAL_PATH, Sim, SimConfig, approval_spec};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const STEPS: usize = 8_000;
const LINK_LINE: u32 = 6;

fn sha256(bytes: &[u8]) -> String {
    let hex: String = Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("sha256:{hex}")
}

fn spec_linking(links: &str) -> String {
    approval_spec("", &[("Build the base", "AC-01, AC-02")])
        .replace("# Approval\n", &format!("# Approval\n\n{links}\n"))
}

fn two_step_spec_linking(links: &str) -> String {
    approval_spec(
        "",
        &[("Build the base", "AC-01"), ("Finish the rest", "AC-02")],
    )
    .replace("# Approval\n", &format!("# Approval\n\n{links}\n"))
}

fn context_of(sim: &Sim, revision: SpecRevisionId) -> Vec<ContextRef> {
    sim.engine()
        .store()
        .read(|c| read::revision_context(c, revision))
        .unwrap()
}

fn count(sim: &Sim, sql: &str) -> i64 {
    sim.engine()
        .store()
        .read(|c| Ok(c.query_row(sql, [], |r| r.get(0))?))
        .unwrap()
}

fn context_hash(sim: &Sim, revision: SpecRevisionId) -> String {
    sim.engine()
        .store()
        .read(|c| read::load_revision(c, revision))
        .unwrap()
        .unwrap()
        .context_hash
}

fn link(path: &str, line: u32, content: &[u8]) -> ContextRef {
    ContextRef {
        path: path.into(),
        origin: ContextOrigin::Link,
        line,
        bytes: content.len() as u64,
        content_hash: sha256(content),
    }
}

#[test]
fn linked_and_always_docs_are_pinned_in_order_and_share_their_blobs() {
    let sim = Sim::new(SimConfig::default());
    let arch = b"# Architecture\n".as_slice();
    let glossary = b"---\ncontext: always\n---\n# Glossary\n".as_slice();
    let conventions = b"---\ncontext: always\n---\n# Conventions\n".as_slice();
    sim.world.put_doc("docs/architecture.md", arch);
    sim.world.put_doc("docs/glossary.md", glossary);
    sim.world.put_doc("docs/team/conventions.md", conventions);
    sim.world
        .put_doc("other/elsewhere.md", b"---\ncontext: always\n---\n");
    sim.world.put_doc(
        "docs/specs/other.md",
        b"---\nsaka: spec\ncontext: always\n---\n",
    );
    sim.world
        .put_doc("docs/notes.md", b"context: always is mentioned here\n");

    let text = spec_linking("See [arch](../architecture.md) and [terms](../glossary.md#a).");
    let first = sim.register(APPROVAL_PATH, &text);
    assert!(first.lint_ok, "{:?}", first.issues);
    let expected = vec![
        link("docs/architecture.md", LINK_LINE, arch),
        link("docs/glossary.md", LINK_LINE, glossary),
        ContextRef {
            path: "docs/team/conventions.md".into(),
            origin: ContextOrigin::Always,
            line: 1,
            bytes: conventions.len() as u64,
            content_hash: sha256(conventions),
        },
    ];
    assert_eq!(context_of(&sim, first.revision_id), expected);

    let view = sim
        .engine()
        .spec(first.spec_id)
        .unwrap()
        .unwrap()
        .revision
        .context;
    assert_eq!(view, expected);

    let second = sim.register(APPROVAL_PATH, &format!("{text}\nOne more line.\n"));
    assert_ne!(second.revision_id, first.revision_id);
    assert_eq!(context_of(&sim, second.revision_id), expected);
    assert_eq!(count(&sim, "SELECT COUNT(*) FROM context_blob"), 3);
    assert_eq!(count(&sim, "SELECT COUNT(*) FROM spec_revision_context"), 6);
}

struct Case {
    name: &'static str,
    docs: Vec<(String, Vec<u8>)>,
    links: String,
    code: &'static str,
    line: u32,
    params: Value,
}

fn cases() -> Vec<Case> {
    let many: Vec<(String, Vec<u8>)> = (0..33)
        .map(|i| (format!("docs/many/{i:02}.md"), b"# x\n".to_vec()))
        .collect();
    let many_links: String = (0..33)
        .map(|i| format!("[d](../many/{i:02}.md) "))
        .collect();
    let half = 128 * 1024 + 1;
    let always: Vec<(String, Vec<u8>)> = (0..65)
        .map(|i| {
            (
                format!("docs/always/{i:02}.md"),
                b"---\ncontext: always\n---\n".to_vec(),
            )
        })
        .collect();
    vec![
        Case {
            name: "outside",
            docs: vec![],
            links: "[x](../../../x.md)".into(),
            code: "reference_outside_workspace",
            line: LINK_LINE,
            params: json!({ "target": "../../../x.md" }),
        },
        Case {
            name: "missing",
            docs: vec![],
            links: "[x](../gone.md)".into(),
            code: "reference_missing",
            line: LINK_LINE,
            params: json!({ "path": "docs/gone.md" }),
        },
        Case {
            name: "not text",
            docs: vec![("docs/bin.md".into(), vec![0xff, 0xfe, 0x00])],
            links: "[x](../bin.md)".into(),
            code: "reference_not_text",
            line: LINK_LINE,
            params: json!({ "path": "docs/bin.md", "origin": "link" }),
        },
        Case {
            name: "too large",
            docs: vec![("docs/big.md".into(), vec![b'x'; 256 * 1024 + 1])],
            links: "[x](../big.md)".into(),
            code: "reference_too_large",
            line: LINK_LINE,
            params: json!({ "path": "docs/big.md", "origin": "link", "bytes": 256 * 1024 + 1, "limit": 256 * 1024 }),
        },
        Case {
            name: "too many docs",
            docs: many,
            links: many_links,
            code: "context_too_large",
            line: 1,
            params: json!({ "reason": "docs", "count": 33, "limit": 32 }),
        },
        Case {
            name: "too many bytes",
            docs: vec![
                ("docs/a.md".into(), vec![b'a'; half]),
                ("docs/b.md".into(), vec![b'b'; half]),
            ],
            links: "[a](../a.md) [b](../b.md)".into(),
            code: "context_too_large",
            line: 1,
            params: json!({ "reason": "bytes", "bytes": 2 * half, "limit": 256 * 1024 }),
        },
        Case {
            name: "too many always candidates",
            docs: always,
            links: String::new(),
            code: "context_too_large",
            line: 1,
            params: json!({ "reason": "always_candidates", "count": 65, "limit": 64 }),
        },
    ]
}

#[test]
fn every_context_error_blocks_approval_with_its_line_and_params() {
    for case in cases() {
        let sim = Sim::new(SimConfig::default());
        for (path, bytes) in &case.docs {
            sim.world.put_doc(path, bytes);
        }
        let registered = sim.register(APPROVAL_PATH, &spec_linking(&case.links));
        assert!(!registered.lint_ok, "{}", case.name);
        let issue = registered
            .issues
            .iter()
            .find(|i| i.code.as_str() == case.code)
            .unwrap_or_else(|| panic!("{}: {:?}", case.name, registered.issues));
        assert!(issue.is_error(), "{}", case.name);
        assert_eq!(issue.line, case.line, "{}", case.name);
        assert_eq!(
            Value::Object(issue.params.clone()),
            case.params,
            "{}",
            case.name
        );

        let err = sim
            .approve_spec(registered.spec_id, registered.revision_id)
            .unwrap_err();
        let EngineError::Rejected(Rejection::SpecNotReady { issues }) = err else {
            panic!("{}: {err:?}", case.name);
        };
        assert!(
            issues
                .as_array()
                .unwrap()
                .iter()
                .any(|i| i["code"] == case.code),
            "{}: {issues}",
            case.name
        );
        if case.code == "context_too_large" {
            assert_eq!(
                count(&sim, "SELECT COUNT(*) FROM spec_revision_context"),
                0,
                "{}",
                case.name
            );
        }
    }
}

#[test]
fn links_to_files_that_are_not_docs_only_warn() {
    let mut sim = Sim::new(SimConfig::default());
    let registered = sim.register(
        APPROVAL_PATH,
        &spec_linking("Touch [the manifest](../../Cargo.toml) and [src](../../src/)."),
    );
    assert!(registered.lint_ok, "{:?}", registered.issues);
    let warnings: Vec<(&str, u32, Value)> = registered
        .issues
        .iter()
        .map(|i| (i.code.as_str(), i.line, Value::Object(i.params.clone())))
        .collect();
    assert_eq!(
        warnings,
        vec![
            (
                "reference_not_doc",
                LINK_LINE,
                json!({ "path": "Cargo.toml" })
            ),
            ("reference_not_doc", LINK_LINE, json!({ "path": "src" })),
        ]
    );
    assert!(context_of(&sim, registered.revision_id).is_empty());
    sim.approve_spec(registered.spec_id, registered.revision_id)
        .unwrap();
    assert!(sim.run_until(STEPS, |s| {
        s.tasks()
            .iter()
            .all(|t| t.item.state == WorkItemState::Done)
    }));
}

#[test]
fn a_changed_doc_makes_a_new_revision_that_repins_only_steps_not_started() {
    let mut sim = Sim::new(SimConfig {
        max_active_tasks: 1,
        ..SimConfig::default()
    });
    sim.world
        .put_doc("docs/architecture.md", b"# Architecture v1\n");
    let text = two_step_spec_linking("See [arch](../architecture.md).");
    let first = sim.register(APPROVAL_PATH, &text);
    assert!(first.lint_ok, "{:?}", first.issues);
    let again = sim.register(APPROVAL_PATH, &text);
    assert_eq!(again.revision_id, first.revision_id);

    sim.approve_spec(first.spec_id, first.revision_id).unwrap();
    let state = |s: &Sim, n: usize| s.tasks_of(first.spec_id)[n - 1].item.state;
    assert!(sim.run_until(STEPS, |s| state(s, 1) == WorkItemState::InStage));
    assert_eq!(state(&sim, 2), WorkItemState::Queued);

    sim.world
        .put_doc("docs/architecture.md", b"# Architecture v2\n");
    let second = sim.register(APPROVAL_PATH, &text);
    assert_ne!(second.revision_id, first.revision_id);
    assert_ne!(
        context_hash(&sim, second.revision_id),
        context_hash(&sim, first.revision_id)
    );
    assert_eq!(
        context_of(&sim, second.revision_id),
        vec![link(
            "docs/architecture.md",
            LINK_LINE,
            b"# Architecture v2\n"
        )]
    );

    sim.approve_spec(first.spec_id, second.revision_id).unwrap();
    let pinned: Vec<_> = sim
        .tasks_of(first.spec_id)
        .iter()
        .map(|t| t.item.spec_revision_id)
        .collect();
    assert_eq!(
        pinned,
        vec![Some(first.revision_id), Some(second.revision_id)]
    );
}
