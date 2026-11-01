use saka_spec::{LintCode, MAX_SPEC_BYTES, parse};
use serde_json::json;

macro_rules! fixture {
    ($name:literal) => {
        include_bytes!(concat!("fixtures/", $name, ".md")).as_slice()
    };
}

#[test]
fn two_step_spec_parses_into_the_model() {
    let parsed = parse("docs/specs/tasks-done.md", fixture!("two-step"));
    assert!(parsed.is_ready(), "{:?}", parsed.issues);
    let model = parsed.model.unwrap();
    assert_eq!(model.slug, "tasks-done");
    assert_eq!(model.title, "Mark tasks as done");

    let acs: Vec<(&str, &str, u32)> = model
        .acceptance_criteria
        .iter()
        .map(|a| (a.id.as_str(), a.text.as_str(), a.line))
        .collect();
    assert_eq!(
        acs,
        vec![
            (
                "AC-01",
                "`tasks done <id>` marks the task done and `tasks list` shows `[x]`.",
                11
            ),
            (
                "AC-02",
                "the web page shows done tasks with a check mark.",
                12
            ),
        ]
    );

    assert_eq!(model.steps.len(), 2);
    let s1 = &model.steps[0];
    assert_eq!((s1.id.as_str(), s1.index, s1.line), ("S1", 1, 19));
    assert_eq!(s1.work, "Add a `done` command to the CLI");
    assert_eq!(s1.ac, vec!["AC-01"]);
    assert_eq!(s1.commands, vec!["cd cli && go test ./..."]);
    let s2 = &model.steps[1];
    assert_eq!((s2.id.as_str(), s2.index), ("S2", 2));
    assert_eq!(s2.ac, vec!["AC-01", "AC-02"]);
    assert_eq!(
        s2.commands,
        vec!["cd web && npm test", "cd cli && go vet ./..."]
    );
}

#[test]
fn crlf_line_endings_parse_the_same() {
    let lf = std::str::from_utf8(fixture!("two-step")).unwrap();
    let crlf = lf.replace('\n', "\r\n");
    let a = parse("tasks-done.md", lf.as_bytes());
    let b = parse("tasks-done.md", crlf.as_bytes());
    assert_eq!(a, b);
}

#[test]
fn each_lint_code_has_a_fixture() {
    let cases: &[(&str, &[u8], LintCode, u32)] = &[
        ("x.md", fixture!("not_a_spec"), LintCode::NotASpec, 1),
        (
            "Bad_Slug.md",
            fixture!("Bad_Slug"),
            LintCode::SlugInvalid,
            1,
        ),
        ("x.md", fixture!("title_missing"), LintCode::TitleMissing, 1),
        (
            "x.md",
            fixture!("ac_section_missing"),
            LintCode::AcSectionMissing,
            1,
        ),
        ("x.md", fixture!("ac_id_invalid"), LintCode::AcIdInvalid, 9),
        (
            "x.md",
            fixture!("ac_id_duplicate"),
            LintCode::AcIdDuplicate,
            9,
        ),
        (
            "x.md",
            fixture!("steps_missing"),
            LintCode::StepsMissing,
            10,
        ),
        (
            "x.md",
            fixture!("steps_column_missing"),
            LintCode::StepsColumnMissing,
            12,
        ),
        (
            "x.md",
            fixture!("steps_column_unsupported"),
            LintCode::StepsColumnUnsupported,
            12,
        ),
        (
            "x.md",
            fixture!("step_id_invalid"),
            LintCode::StepIdInvalid,
            15,
        ),
        (
            "x.md",
            fixture!("step_limit_exceeded"),
            LintCode::StepLimitExceeded,
            64,
        ),
        ("x.md", fixture!("step_ac_empty"), LintCode::StepAcEmpty, 14),
        (
            "x.md",
            fixture!("step_ac_unknown"),
            LintCode::StepAcUnknown,
            14,
        ),
        (
            "x.md",
            fixture!("step_command_missing"),
            LintCode::StepCommandMissing,
            14,
        ),
    ];
    for (name, bytes, code, line) in cases {
        let parsed = parse(name, bytes);
        let found: Vec<(LintCode, u32)> = parsed.issues.iter().map(|i| (i.code, i.line)).collect();
        assert_eq!(found, vec![(*code, *line)], "fixture for {}", code.as_str());
        assert!(!parsed.is_ready());
    }
}

#[test]
fn issue_params_name_what_is_wrong() {
    let unknown = parse("x.md", fixture!("step_ac_unknown"));
    assert_eq!(
        serde_json::Value::Object(unknown.issues[0].params.clone()),
        json!({"step": "S1", "ac": "AC-09"})
    );
    let id = parse("x.md", fixture!("step_id_invalid"));
    assert_eq!(
        serde_json::Value::Object(id.issues[0].params.clone()),
        json!({"expected": "S2", "found": "S3"})
    );
    let column = parse("x.md", fixture!("steps_column_missing"));
    assert_eq!(
        serde_json::Value::Object(column.issues[0].params.clone()),
        json!({"column": "ac"})
    );
}

#[test]
fn oversized_file_is_rejected_before_parsing() {
    let big = vec![b'a'; MAX_SPEC_BYTES + 1];
    let parsed = parse("x.md", &big);
    assert_eq!(parsed.model, None);
    assert_eq!(parsed.issues[0].code, LintCode::FileTooLarge);
    assert_eq!(parsed.issues[0].line, 1);
}

#[test]
fn invalid_utf8_reports_its_line() {
    let mut bytes = b"---\nsaka: spec\n---\n# T\n".to_vec();
    bytes.extend_from_slice(&[0xff, 0xfe]);
    let parsed = parse("x.md", &bytes);
    assert_eq!(parsed.model, None);
    assert_eq!(
        parsed
            .issues
            .iter()
            .map(|i| (i.code, i.line))
            .collect::<Vec<_>>(),
        vec![(LintCode::InvalidUtf8, 5)]
    );
}

#[test]
fn issue_codes_serialize_as_snake_case() {
    assert_eq!(
        serde_json::to_value(LintCode::StepAcUnknown).unwrap(),
        json!("step_ac_unknown")
    );
    assert_eq!(LintCode::StepAcUnknown.as_str(), "step_ac_unknown");
}
