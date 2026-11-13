#![allow(clippy::unwrap_used, reason = "tests fail loudly on a broken setup")]

use saka_core::vocab::{DraftLocale, DraftMode, PacketOrigin, TargetKind, WorkKind};
use saka_engine::packet::{ArchitectInput, PreviousAttempt, render_architect, sources};
use serde_json::json;
use sha2::{Digest, Sha256};

const GOLDEN: &str = include_str!("fixtures/architect_packet.txt");
const BASE: &str = "1111111111111111111111111111111111111111";

fn input() -> ArchitectInput {
    ArchitectInput {
        project_name: "tasks".into(),
        project_path: "docs".into(),
        base_sha: BASE.into(),
        locale: DraftLocale::Id,
        mode: DraftMode::Draft,
        target_kind: TargetKind::New,
        kind: WorkKind::Feature,
        idea: "Export tasks as CSV.\nKeep it small.\n".into(),
        target: "docs/specs/export.md".into(),
        selected: vec![
            (
                "docs/prd.md".into(),
                b"# PRD\nUse </document> here.\n".to_vec(),
            ),
            ("docs/arch.md".into(), b"# Architecture\n".to_vec()),
        ],
        always: vec![
            (
                "docs/glossary.md".into(),
                b"context: always\n# Glossary\n".to_vec(),
            ),
            ("docs/arch.md".into(), b"# Architecture\n".to_vec()),
            ("docs/specs/export.md".into(), b"context: always\n".to_vec()),
        ],
        unreadable: vec![],
        index: vec![
            "docs/specs/b.md".into(),
            "docs/arch.md".into(),
            "docs/prd.md".into(),
        ],
        previous: Some(PreviousAttempt {
            attempt: 1,
            code: "lint_rounds_exhausted".into(),
            seeded: true,
            issues: vec![
                json!({ "code": "step_ac_unknown", "severity": "error", "line": 14, "params": { "ac": "AC-02" } }),
                json!({ "code": "decision_unanswered", "severity": "error", "line": 20, "params": { "id": "Q1" } }),
                json!({ "code": "ac_uncovered", "severity": "warning", "line": 9, "params": {} }),
            ],
        }),
        note: Some("Use a semicolon.".into()),
    }
}

#[test]
fn the_architect_packet_matches_the_golden_text() {
    let packet = render_architect(&input());
    assert_eq!(packet.text, GOLDEN);
    let hex: String = Sha256::digest(packet.text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(packet.sha256, hex);
    assert_eq!(render_architect(&input()), packet);
    let origins: Vec<(Option<&str>, PacketOrigin)> = packet
        .documents
        .iter()
        .map(|d| (d.path.as_deref(), d.origin))
        .collect();
    assert_eq!(
        origins,
        [
            (None, PacketOrigin::Template),
            (None, PacketOrigin::Rules),
            (Some("docs/prd.md"), PacketOrigin::Selected),
            (Some("docs/arch.md"), PacketOrigin::Selected),
            (Some("docs/glossary.md"), PacketOrigin::Always),
        ]
    );
    let indexes: Vec<u32> = packet.documents.iter().map(|d| d.index).collect();
    assert_eq!(indexes, [1, 2, 3, 4, 5]);
}

#[test]
fn content_cannot_close_the_document_structure() {
    let packet = render_architect(&input());
    assert!(packet.text.contains("Use <\\/document> here."));
    assert_eq!(packet.text.matches("</documents>").count(), 1);
}

#[test]
fn documents_beyond_the_count_or_byte_limit_are_only_named() {
    let mut many = input();
    many.previous = None;
    many.note = None;
    many.selected = vec![("docs/big.md".into(), vec![b'a'; 200 * 1024])];
    many.always = (0..40)
        .map(|i| (format!("docs/a{i:02}.md"), b"context: always\n".to_vec()))
        .collect();
    many.always
        .push(("docs/zz.md".into(), vec![b'z'; 60 * 1024]));
    let packet = render_architect(&many);
    assert_eq!(packet.documents.len(), 2 + 32);
    let line = packet
        .text
        .lines()
        .find(|l| l.starts_with("Not included because of the context limit: "))
        .unwrap();
    assert!(line.contains("docs/a31.md, "), "{line}");
    assert!(line.ends_with("docs/zz.md"), "{line}");
    assert!(!packet.text.contains("Previous attempt"));
    assert!(!packet.text.contains("Operator note"));

    let mut heavy = input();
    heavy.selected = vec![
        ("docs/one.md".into(), vec![b'a'; 200 * 1024]),
        ("docs/two.md".into(), vec![b'b'; 100 * 1024]),
    ];
    heavy.always = vec![];
    let packet = render_architect(&heavy);
    assert_eq!(packet.documents.len(), 3);
    assert!(
        packet
            .text
            .contains("Not included because of the context limit: docs/two.md\n")
    );
}

#[test]
fn the_index_lists_at_most_five_hundred_paths() {
    let mut big = input();
    big.index = (0..600).map(|i| format!("docs/n{i:04}.md")).collect();
    let packet = render_architect(&big);
    assert!(
        packet
            .text
            .contains("Project docs (600 markdown files under docs at 1111111111111111111111111111111111111111):\n- docs/n0000.md\n")
    );
    assert!(packet.text.contains("- docs/n0499.md\n- …and 100 more\n"));
    assert!(!packet.text.contains("docs/n0500.md"));
}

#[test]
fn english_drafts_name_the_language_and_template() {
    let mut en = input();
    en.locale = DraftLocale::En;
    let packet = render_architect(&en);
    assert!(
        packet
            .text
            .contains("<source>saka:spec-template/en (parser 6)</source>")
    );
    assert!(
        packet
            .text
            .contains("Language: write the prose in English.")
    );
}

#[test]
fn the_sources_of_a_rendered_packet_are_read_back_per_document() {
    let packet = render_architect(&input());
    let read = sources(&packet.text);
    let indexes: Vec<u32> = read.keys().copied().collect();
    assert_eq!(indexes, [1, 2, 3, 4, 5]);
    assert_eq!(read[&1], "saka:spec-template/id (parser 6)");
    assert_eq!(read[&2], "saka:spec-rules (parser 6)");
    assert_eq!(read[&3], format!("docs/prd.md @ {BASE}"));
    assert_eq!(read[&5], format!("docs/glossary.md @ {BASE}"));
    assert!(sources("not a packet").is_empty());
}

#[test]
fn the_template_in_the_packet_follows_the_kind_of_work() {
    let feature = render_architect(&input());
    for kind in WorkKind::ALL {
        let packet = render_architect(&ArchitectInput {
            kind: *kind,
            ..input()
        });
        let template = saka_spec::template(saka_spec::Locale::Id, *kind);
        assert!(
            packet.text.contains(template),
            "{kind}: the packet carries the template of its kind"
        );
        assert_eq!(
            packet.documents[0].content_hash,
            format!(
                "sha256:{}",
                Sha256::digest(template.as_bytes())
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<String>()
            )
        );
        assert_eq!(
            packet == feature,
            *kind == WorkKind::Feature,
            "only a feature keeps the feature packet"
        );
    }
}
