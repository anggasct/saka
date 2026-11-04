use saka_spec::{
    Decision, LintCode, LintIssue, Locale, MAX_DECISIONS, MAX_SPEC_BYTES, PARSER_VERSION, Severity,
    parse, rules, template,
};
use serde_json::{Value, json};

macro_rules! fixture {
    ($name:literal) => {
        include_bytes!(concat!("fixtures/", $name, ".md")).as_slice()
    };
}

fn spec_with_frontmatter(lines: &str) -> String {
    format!(
        "---\nsaka: spec\n{lines}---\n# Title\n\n## Acceptance criteria\n\n- **AC-01**: one.\n\n## Steps\n\n| Step | Work | AC | Done when |\n|------|------|----|-----------|\n| S1 | x | AC-01 | `true` |\n"
    )
}

fn found(issues: &[LintIssue]) -> Vec<(LintCode, Severity, u32, Value)> {
    issues
        .iter()
        .map(|i| (i.code, i.severity, i.line, Value::Object(i.params.clone())))
        .collect()
}

fn after(parsed: &saka_spec::Parsed) -> Vec<(String, Vec<String>)> {
    parsed
        .model
        .iter()
        .flat_map(|m| &m.steps)
        .map(|s| (s.id.clone(), s.after.clone()))
        .collect()
}

#[test]
fn the_parser_is_version_five() {
    assert_eq!(PARSER_VERSION, 5);
}

fn links_of(text: &str) -> (Vec<(String, u32)>, Vec<LintIssue>) {
    let parsed = parse("docs/specs/t.md", text.as_bytes());
    let links = parsed
        .model
        .map(|m| m.links.into_iter().map(|l| (l.target, l.line)).collect())
        .unwrap_or_default();
    (links, parsed.issues)
}

#[test]
fn inline_and_reference_links_are_recorded_in_document_order() {
    let body = "See [arch](../arch.md#layers) and [ref][r].\n\n\
                [Web](https://example.com), <https://x.dev>, [mail](mailto:a@b.c), [host](//h/x.md),\n\
                [self](#acceptance-criteria), ![img](../img.md), `[code](../code.md)`, [q](notes.md?plain=1).\n\n\
                ```\n[block](../block.md)\n```\n\n\
                [space](my%20doc.md) [bad](%ff.md)\n\n\
                [r]: /docs/ref.md\n";
    let text = spec_with_frontmatter("").replace("# Title\n", &format!("# Title\n\n{body}"));
    let (links, issues) = links_of(&text);
    assert!(issues.is_empty(), "{issues:?}");
    assert_eq!(
        links,
        vec![
            ("../arch.md".to_string(), 6),
            ("/docs/ref.md".to_string(), 6),
            ("notes.md".to_string(), 9),
            ("my doc.md".to_string(), 15),
            ("%ff.md".to_string(), 15),
        ]
    );
}

#[test]
fn links_in_the_frontmatter_are_not_recorded() {
    let text = spec_with_frontmatter("title: \"[x](a.md)\"\n");
    assert_eq!(links_of(&text).0, Vec::new());
}

#[test]
fn links_past_the_limit_warn_once() {
    let many: String = (0..300).map(|i| format!("[d{i}](d{i}.md)\n\n")).collect();
    let text = spec_with_frontmatter("").replace("# Title\n", &format!("# Title\n\n{many}"));
    let (links, issues) = links_of(&text);
    assert_eq!(links.len(), 256);
    assert_eq!(links[255].0, "d255.md");
    let found = found(&issues);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].0, LintCode::LinksLimitExceeded);
    assert_eq!(found[0].1, Severity::Warning);
    assert_eq!(found[0].2, 6 + 2 * 256);
    assert_eq!(found[0].3, json!({ "limit": 256 }));
}

#[test]
fn two_step_spec_parses_into_the_model() {
    let parsed = parse("docs/specs/tasks-done.md", fixture!("two-step"));
    assert!(parsed.is_ready(), "{:?}", parsed.issues);
    assert!(parsed.issues.is_empty(), "{:?}", parsed.issues);
    let model = parsed.model.unwrap();
    assert_eq!(model.slug, "tasks-done");
    assert_eq!(model.title, "Mark tasks as done");
    assert_eq!(model.priority, 0);
    assert!(model.depends_on.is_empty());

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
    assert!(s1.after.is_empty());
    let s2 = &model.steps[1];
    assert_eq!((s2.id.as_str(), s2.index), ("S2", 2));
    assert_eq!(s2.ac, vec!["AC-01", "AC-02"]);
    assert_eq!(
        s2.commands,
        vec!["cd web && npm test", "cd cli && go vet ./..."]
    );
    assert_eq!(s2.after, vec!["S1"]);
    assert!(model.steps.iter().all(|s| s.repo.is_none()));
}

#[test]
fn the_repo_column_names_each_step_repository() {
    let parsed = parse("x.md", fixture!("step_repo_invalid"));
    let repos: Vec<Option<&str>> = parsed
        .model
        .iter()
        .flat_map(|m| &m.steps)
        .map(|s| s.repo.as_deref())
        .collect();
    assert_eq!(repos, vec![Some("web"), None, Some("Web!")]);
    assert_eq!(
        found(&parsed.issues),
        vec![(
            LintCode::StepRepoInvalid,
            Severity::Error,
            16,
            json!({"step": "S3", "repo": "Web!"})
        )]
    );
    let model = serde_json::to_value(parsed.model.unwrap()).unwrap();
    assert_eq!(model["steps"][0]["repo"], json!("web"));
    assert_eq!(model["steps"][1]["repo"], Value::Null);
    let text = std::str::from_utf8(fixture!("step_repo_invalid"))
        .unwrap()
        .replace("| Web! |", "| a-32-character-repo-name-is-okay |");
    assert!(parse("x.md", text.as_bytes()).issues.is_empty());
    let long = text.replace("a-32-character-repo-name-is-okay", &"r".repeat(33));
    assert_eq!(
        parse("x.md", long.as_bytes()).issues[0].code,
        LintCode::StepRepoInvalid
    );
}

#[test]
fn a_done_when_spec_uses_the_default_repo() {
    let parsed = parse("x.md", fixture!("one-step"));
    assert!(parsed.model.unwrap().steps.iter().all(|s| s.repo.is_none()));
}

#[test]
fn crlf_line_endings_parse_the_same() {
    for bytes in [
        fixture!("two-step"),
        fixture!("frontmatter-full"),
        fixture!("decisions-ok"),
    ] {
        let lf = std::str::from_utf8(bytes).unwrap();
        let crlf = lf.replace('\n', "\r\n");
        assert_eq!(parse("x.md", lf.as_bytes()), parse("x.md", crlf.as_bytes()));
    }
}

#[test]
fn each_lint_code_has_a_fixture() {
    use LintCode as C;
    let skip = Value::Null;
    let cases: Vec<(&str, &[u8], LintCode, u32, Value)> = vec![
        ("x.md", fixture!("not_a_spec"), C::NotASpec, 1, json!({})),
        (
            "Bad_Slug.md",
            fixture!("Bad_Slug"),
            C::SlugInvalid,
            1,
            json!({"slug": "Bad_Slug"}),
        ),
        (
            "x.md",
            fixture!("title_missing"),
            C::TitleMissing,
            1,
            json!({}),
        ),
        (
            "x.md",
            fixture!("ac_section_missing"),
            C::AcSectionMissing,
            1,
            json!({}),
        ),
        (
            "x.md",
            fixture!("ac_id_invalid"),
            C::AcIdInvalid,
            9,
            json!({"id": "AC-1"}),
        ),
        (
            "x.md",
            fixture!("ac_id_duplicate"),
            C::AcIdDuplicate,
            9,
            skip.clone(),
        ),
        (
            "x.md",
            fixture!("steps_missing"),
            C::StepsMissing,
            10,
            json!({}),
        ),
        (
            "x.md",
            fixture!("steps_column_missing"),
            C::StepsColumnMissing,
            12,
            json!({"column": "ac"}),
        ),
        (
            "x.md",
            fixture!("step_repo_invalid"),
            C::StepRepoInvalid,
            16,
            json!({"step": "S3", "repo": "Web!"}),
        ),
        (
            "x.md",
            fixture!("step_id_invalid"),
            C::StepIdInvalid,
            15,
            json!({"expected": "S2", "found": "S3"}),
        ),
        (
            "x.md",
            fixture!("step_limit_exceeded"),
            C::StepLimitExceeded,
            64,
            json!({"max_steps": 50}),
        ),
        (
            "x.md",
            fixture!("step_ac_empty"),
            C::StepAcEmpty,
            14,
            json!({"step": "S1"}),
        ),
        (
            "x.md",
            fixture!("step_ac_unknown"),
            C::StepAcUnknown,
            14,
            json!({"step": "S1", "ac": "AC-09"}),
        ),
        (
            "x.md",
            fixture!("step_command_missing"),
            C::StepCommandMissing,
            14,
            json!({"step": "S1"}),
        ),
        (
            "x.md",
            fixture!("frontmatter_invalid"),
            C::FrontmatterInvalid,
            3,
            json!({"reason": "not_a_key", "key": "Owner"}),
        ),
        (
            "x.md",
            fixture!("frontmatter_duplicate_key"),
            C::FrontmatterInvalid,
            4,
            json!({"reason": "duplicate_key", "key": "priority"}),
        ),
        (
            "x.md",
            fixture!("priority_invalid"),
            C::PriorityInvalid,
            3,
            json!({"value": "high"}),
        ),
        (
            "x.md",
            fixture!("depends_on_invalid"),
            C::DependsOnInvalid,
            3,
            json!({"value": "Not_A_Slug"}),
        ),
        (
            "x.md",
            fixture!("depends_on_self"),
            C::DependsOnSelf,
            5,
            json!({"slug": "x"}),
        ),
        (
            "x.md",
            fixture!("step_after_unknown"),
            C::StepAfterUnknown,
            15,
            json!({"step": "S2", "after": "S7"}),
        ),
        (
            "x.md",
            fixture!("step_after_cycle"),
            C::StepAfterCycle,
            15,
            json!({"steps": ["S2", "S3"]}),
        ),
        (
            "x.md",
            fixture!("done_when_with_steps"),
            C::DoneWhenWithSteps,
            17,
            json!({}),
        ),
        (
            "x.md",
            fixture!("done_when_empty"),
            C::StepCommandMissing,
            11,
            json!({"step": "S1"}),
        ),
        (
            "x.md",
            fixture!("ac_uncovered"),
            C::AcUncovered,
            9,
            json!({"ac": "AC-02"}),
        ),
        (
            "x.md",
            fixture!("step_work_empty"),
            C::StepWorkEmpty,
            15,
            json!({"step": "S1"}),
        ),
        (
            "x.md",
            fixture!("step_command_duplicate"),
            C::StepCommandDuplicate,
            15,
            json!({"step": "S1", "command": "true"}),
        ),
        (
            "x.md",
            fixture!("step_parallel_start"),
            C::StepParallelStart,
            15,
            json!({"step": "S2"}),
        ),
    ];
    for (name, bytes, code, line, params) in cases {
        let parsed = parse(name, bytes);
        let mut issues = found(&parsed.issues);
        if params.is_null() {
            for issue in &mut issues {
                issue.3 = Value::Null;
            }
        }
        assert_eq!(
            issues,
            vec![(code, code.severity(), line, params)],
            "fixture for {}",
            code.as_str()
        );
        assert_eq!(parsed.is_ready(), code.severity() == Severity::Warning);
    }
}

#[test]
fn every_lint_code_has_a_fixed_severity() {
    use LintCode as C;
    let warnings = [
        C::AcUncovered,
        C::StepWorkEmpty,
        C::StepCommandDuplicate,
        C::StepParallelStart,
        C::LinksLimitExceeded,
        C::ReferenceNotDoc,
    ];
    for code in LintCode::ALL {
        let expected = if warnings.contains(&code) {
            Severity::Warning
        } else {
            Severity::Error
        };
        assert_eq!(code.severity(), expected, "{}", code.as_str());
        assert_eq!(serde_json::to_value(code).unwrap(), json!(code.as_str()));
        assert_eq!(LintIssue::new(code, 1).severity, expected);
    }
    let mut names: Vec<&str> = LintCode::ALL.iter().map(|c| c.as_str()).collect();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), LintCode::ALL.len());
}

#[test]
fn issues_serialize_with_their_severity_and_old_issues_read_as_errors() {
    let issue = LintIssue::new(LintCode::AcUncovered, 9).with("ac", "AC-02");
    assert_eq!(
        serde_json::to_value(&issue).unwrap(),
        json!({"code": "ac_uncovered", "severity": "warning", "line": 9, "params": {"ac": "AC-02"}})
    );
    let old: LintIssue = serde_json::from_value(
        json!({"code": "step_ac_empty", "line": 14, "params": {"step": "S1"}}),
    )
    .unwrap();
    assert_eq!(old.severity, Severity::Error);
}

#[test]
fn frontmatter_reads_dependencies_and_priority() {
    let cases: &[(&str, i32, &[&str])] = &[
        ("", 0, &[]),
        (
            "depends_on: [auth-model, billing]\n",
            0,
            &["auth-model", "billing"],
        ),
        (
            "depends_on: ['auth-model', \"billing\"]\n",
            0,
            &["auth-model", "billing"],
        ),
        ("depends_on: []\n", 0, &[]),
        (
            "depends_on:\n  - auth-model\n  - billing\n",
            0,
            &["auth-model", "billing"],
        ),
        ("depends_on: [a, b, a]\n", 0, &["a", "b"]),
        ("priority: 10\n", 10, &[]),
        ("priority: -1000\n", -1000, &[]),
        ("priority: \"1000\"   # quoted\n", 1000, &[]),
        ("# priority: 5\nowner: me\n", 0, &[]),
        (
            "meta:\n  nested:\n    - x: [y]\n  other: 'a # b'\npriority: 3\n",
            3,
            &[],
        ),
        (
            "title: 'free: text'\nurl: https://example.com/a#b\n",
            0,
            &[],
        ),
    ];
    for (lines, priority, depends_on) in cases {
        let text = spec_with_frontmatter(lines);
        let parsed = parse("x.md", text.as_bytes());
        assert!(parsed.issues.is_empty(), "{lines:?}: {:?}", parsed.issues);
        let model = parsed.model.unwrap();
        assert_eq!(model.priority, *priority, "{lines:?}");
        assert_eq!(model.depends_on, *depends_on, "{lines:?}");
    }

    let full = parse("x.md", fixture!("frontmatter-full"));
    assert!(full.issues.is_empty(), "{:?}", full.issues);
    let model = full.model.clone().unwrap();
    assert_eq!(model.priority, 10);
    assert_eq!(model.depends_on, vec!["auth-model", "billing"]);
    assert_eq!(model.title, "Full frontmatter");
    assert_eq!(model.steps[0].line, 26);
    assert_eq!(full.depends_on_line, Some(10));
    assert_eq!(parse("x.md", fixture!("two-step")).depends_on_line, None);
}

#[test]
fn bad_frontmatter_values_are_reported_on_their_line() {
    let cases: &[(&str, LintCode, u32, Value)] = &[
        (
            "priority: 1001\n",
            LintCode::PriorityInvalid,
            3,
            json!({"value": "1001"}),
        ),
        (
            "priority: -1001\n",
            LintCode::PriorityInvalid,
            3,
            json!({"value": "-1001"}),
        ),
        (
            "priority: 1.5\n",
            LintCode::PriorityInvalid,
            3,
            json!({"value": "1.5"}),
        ),
        (
            "priority:\n",
            LintCode::PriorityInvalid,
            3,
            json!({"value": ""}),
        ),
        (
            "depends_on: billing\n",
            LintCode::DependsOnInvalid,
            3,
            json!({"value": "billing"}),
        ),
        (
            "depends_on:\n  billing\n",
            LintCode::DependsOnInvalid,
            4,
            json!({"value": "billing"}),
        ),
        (
            "depends_on: [x]\n",
            LintCode::DependsOnSelf,
            3,
            json!({"slug": "x"}),
        ),
        (
            "key:value\n",
            LintCode::FrontmatterInvalid,
            3,
            json!({"reason": "not_a_key", "key": "key"}),
        ),
        (
            "depends_on: []\ndepends_on: [a]\n",
            LintCode::FrontmatterInvalid,
            4,
            json!({"reason": "duplicate_key", "key": "depends_on"}),
        ),
    ];
    for (lines, code, line, params) in cases {
        let text = spec_with_frontmatter(lines);
        let parsed = parse("x.md", text.as_bytes());
        assert_eq!(
            found(&parsed.issues),
            vec![(*code, Severity::Error, *line, params.clone())],
            "{lines:?}"
        );
    }
}

#[test]
fn an_indented_line_before_any_key_is_not_a_key() {
    let text = spec_with_frontmatter("").replacen("---\n", "---\n  stray: 1\n", 1);
    assert_eq!(
        found(&parse("x.md", text.as_bytes()).issues),
        vec![(
            LintCode::FrontmatterInvalid,
            Severity::Error,
            2,
            json!({"reason": "not_a_key", "key": "stray"})
        )]
    );
}

#[test]
fn frontmatter_without_the_marker_or_a_closing_line_is_not_a_spec() {
    for text in [
        "---\nowner: me\n---\n# T\n",
        "---\nsaka: other\n---\n# T\n",
        "---\nsaka: spec\n# T\n",
        "# T\n---\nsaka: spec\n---\n",
    ] {
        let parsed = parse("x.md", text.as_bytes());
        assert_eq!(parsed.model, None, "{text:?}");
        assert_eq!(found(&parsed.issues)[0].0, LintCode::NotASpec);
    }
    let dots = spec_with_frontmatter("").replacen("---\n# Title", "...\n# Title", 1);
    assert!(parse("x.md", dots.as_bytes()).issues.is_empty());
}

#[test]
fn the_after_column_sets_step_dependencies() {
    let parsed = parse("x.md", fixture!("after-graph"));
    assert!(parsed.issues.is_empty(), "{:?}", parsed.issues);
    let s = |ids: &[&str]| ids.iter().map(|id| id.to_string()).collect::<Vec<_>>();
    assert_eq!(
        after(&parsed),
        vec![
            ("S1".to_string(), s(&[])),
            ("S2".to_string(), s(&["S1"])),
            ("S3".to_string(), s(&["S1"])),
            ("S4".to_string(), s(&["S2", "S3"])),
        ]
    );

    let parallel = parse("x.md", fixture!("step_parallel_start"));
    assert_eq!(
        after(&parallel),
        vec![("S1".to_string(), s(&[])), ("S2".to_string(), s(&[]))]
    );
}

#[test]
fn a_step_waiting_for_itself_is_a_cycle() {
    let text = "---\nsaka: spec\n---\n# T\n\n## Acceptance criteria\n\n- **AC-01**: one.\n\n## Steps\n\n| Step | Work | AC | After | Done when |\n|---|---|---|---|---|\n| S1 | x | AC-01 | S1 | `true` |\n";
    let parsed = parse("x.md", text.as_bytes());
    assert_eq!(
        found(&parsed.issues),
        vec![(
            LintCode::StepAfterCycle,
            Severity::Error,
            14,
            json!({"steps": ["S1"]})
        )]
    );
}

#[test]
fn a_done_when_section_is_a_single_step() {
    let parsed = parse("x.md", fixture!("one-step"));
    assert!(parsed.issues.is_empty(), "{:?}", parsed.issues);
    let model = parsed.model.unwrap();
    assert_eq!(model.steps.len(), 1);
    let step = &model.steps[0];
    assert_eq!((step.id.as_str(), step.index, step.line), ("S1", 1, 11));
    assert_eq!(step.work, "Small fix");
    assert_eq!(step.ac, vec!["AC-01", "AC-02"]);
    assert_eq!(
        step.commands,
        vec!["cd cli && go test ./...", "test -f out.md"]
    );
    assert!(step.after.is_empty());
}

#[test]
fn done_when_items_need_exactly_one_command() {
    let parsed = parse("x.md", fixture!("done_when_item_invalid"));
    assert_eq!(
        found(&parsed.issues),
        vec![
            (
                LintCode::DoneWhenItemInvalid,
                Severity::Error,
                14,
                json!({"item": 2})
            ),
            (
                LintCode::DoneWhenItemInvalid,
                Severity::Error,
                15,
                json!({"item": 3})
            ),
        ]
    );
    assert_eq!(parsed.model.unwrap().steps[0].commands, vec!["true"]);
}

#[test]
fn warnings_alone_leave_the_spec_ready() {
    let text = "---\nsaka: spec\n---\n# T\n\n## Acceptance criteria\n\n- **AC-01**: one.\n- **AC-02**: two.\n\n## Steps\n\n| Step | Work | AC | After | Done when |\n|---|---|---|---|---|\n| S1 |  | AC-01 | | `true` `true` |\n| S2 | y | AC-01 | | `true` |\n";
    let parsed = parse("x.md", text.as_bytes());
    let codes: Vec<LintCode> = parsed.issues.iter().map(|i| i.code).collect();
    assert_eq!(
        codes,
        vec![
            LintCode::AcUncovered,
            LintCode::StepWorkEmpty,
            LintCode::StepCommandDuplicate,
            LintCode::StepParallelStart,
        ]
    );
    assert!(
        parsed
            .issues
            .iter()
            .all(|i| i.severity == Severity::Warning)
    );
    assert!(parsed.is_ready());
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
fn both_templates_parse_without_issues() {
    for locale in Locale::ALL {
        let parsed = parse("example-spec.md", template(locale).as_bytes());
        assert!(
            parsed.issues.is_empty(),
            "{}: {:?}",
            locale.as_str(),
            parsed.issues
        );
        assert!(template(locale).contains("`## Decisions`"));
        let model = parsed.model.unwrap();
        assert!(model.decisions.is_empty());
        assert_eq!(model.steps.len(), 2);
        assert_eq!(model.acceptance_criteria.len(), 2);
    }
    assert_ne!(template(Locale::En), template(Locale::Id));
    assert_eq!(Locale::from_code("id"), Some(Locale::Id));
    assert_eq!(Locale::from_code("fr"), None);
}

#[test]
fn the_end_to_end_fixture_spec_is_ready() {
    let template = include_str!("../../../xtask/src/e2e/fixture-spec.md.tmpl");
    let run_id = "20261101-093000-1a2b";
    let text = template.replace("{run_id}", run_id);
    let parsed = parse(&format!("docs/specs/e2e-{run_id}.md"), text.as_bytes());
    assert!(parsed.issues.is_empty(), "{:?}", parsed.issues);
    let model = parsed.model.unwrap();
    assert_eq!(model.slug, format!("e2e-{run_id}"));
    let steps: Vec<(&str, Vec<&str>)> = model
        .steps
        .iter()
        .map(|s| {
            (
                s.id.as_str(),
                s.commands.iter().map(String::as_str).collect(),
            )
        })
        .collect();
    assert_eq!(
        steps,
        [
            (
                "S1",
                vec![
                    "test -f e2e-runs/20261101-093000-1a2b/s1.md",
                    "cd cli && go test ./..."
                ]
            ),
            ("S2", vec!["test -f e2e-runs/20261101-093000-1a2b/s2.md"]),
        ]
    );
}

fn options_of(decision: &Decision) -> Vec<(&str, u32, bool, bool)> {
    decision
        .options
        .iter()
        .map(|o| (o.text.as_str(), o.line, o.recommended, o.chosen))
        .collect()
}

#[test]
fn decisions_are_read_with_their_options_and_open_ones_are_errors() {
    let parsed = parse("x.md", fixture!("decisions-ok"));
    assert_eq!(
        found(&parsed.issues),
        vec![(
            LintCode::DecisionUnanswered,
            Severity::Error,
            29,
            json!({"id": "Q2"})
        )]
    );
    assert!(!parsed.is_ready());
    let decisions = parsed.model.unwrap().decisions;
    assert_eq!(decisions.len(), 2);
    let q1 = &decisions[0];
    assert_eq!(
        (q1.id.as_str(), q1.question.as_str(), q1.line, q1.chosen),
        ("Q1", "Where are uploaded files stored?", 16, Some(1))
    );
    assert_eq!(
        options_of(q1),
        vec![
            (
                "**Local disk** (recommended) — one node only;\nnothing to configure.",
                20,
                true,
                true
            ),
            (
                "**S3-compatible storage** — scales out; needs credentials.",
                22,
                false,
                false
            ),
            ("**Both, chosen per project** — flexible.", 23, false, false),
        ]
    );
    let q2 = &decisions[1];
    assert_eq!(
        (q2.id.as_str(), q2.question.as_str(), q2.line, q2.chosen),
        ("Q2", "How are thumbnails made?", 29, None)
    );
    assert_eq!(
        options_of(q2),
        vec![
            ("On upload (RECOMMENDED)", 31, true, false),
            ("On first read", 32, false, false),
        ]
    );
    assert_eq!(
        serde_json::to_value(q2).unwrap()["chosen"],
        Value::Null,
        "an open decision serializes its answer as null"
    );
}

#[test]
fn a_capital_x_answers_a_decision() {
    let parsed = parse("x.md", fixture!("decisions-answered"));
    assert!(parsed.issues.is_empty(), "{:?}", parsed.issues);
    assert!(parsed.is_ready());
    let chosen: Vec<Option<u32>> = parsed
        .model
        .unwrap()
        .decisions
        .iter()
        .map(|d| d.chosen)
        .collect();
    assert_eq!(chosen, vec![Some(1), Some(2)]);
}

#[test]
fn each_decision_code_has_a_fixture() {
    use LintCode as C;
    let cases: Vec<(&[u8], LintCode, u32, Value)> = vec![
        (
            fixture!("decision_id_invalid"),
            C::DecisionIdInvalid,
            16,
            json!({"heading": "Storage"}),
        ),
        (
            fixture!("decision_id_duplicate"),
            C::DecisionIdDuplicate,
            21,
            json!({"id": "Q1"}),
        ),
        (
            fixture!("decisions-21"),
            C::DecisionLimitExceeded,
            116,
            json!({"max": 20}),
        ),
        (
            fixture!("decision_options_missing"),
            C::DecisionOptionsInvalid,
            16,
            json!({"id": "Q1", "reason": "missing"}),
        ),
        (
            fixture!("decision_options_count_one"),
            C::DecisionOptionsInvalid,
            16,
            json!({"id": "Q1", "reason": "count", "count": 1}),
        ),
        (
            fixture!("decision_options_count_four"),
            C::DecisionOptionsInvalid,
            16,
            json!({"id": "Q1", "reason": "count", "count": 4}),
        ),
        (
            fixture!("decision_options_not_task"),
            C::DecisionOptionsInvalid,
            19,
            json!({"id": "Q1", "reason": "not_task", "option": 2}),
        ),
        (
            fixture!("decision_recommendation_first_missing"),
            C::DecisionRecommendationInvalid,
            18,
            json!({"id": "Q1", "reason": "first_missing", "option": 1}),
        ),
        (
            fixture!("decision_recommendation_other_marked"),
            C::DecisionRecommendationInvalid,
            19,
            json!({"id": "Q1", "reason": "other_marked", "option": 2}),
        ),
        (
            fixture!("decision_answer_multiple"),
            C::DecisionAnswerMultiple,
            16,
            json!({"id": "Q1"}),
        ),
    ];
    for (bytes, code, line, params) in cases {
        let parsed = parse("x.md", bytes);
        assert_eq!(
            found(&parsed.issues),
            vec![(code, Severity::Error, line, params.clone())],
            "fixture for {} {params}",
            code.as_str()
        );
        assert!(!parsed.is_ready());
    }
    let limited = parse("x.md", fixture!("decisions-21")).model.unwrap();
    assert_eq!(limited.decisions.len(), MAX_DECISIONS);
    assert_eq!(limited.decisions.last().unwrap().id, "Q20");
    let duplicate = parse("x.md", fixture!("decision_id_duplicate"))
        .model
        .unwrap();
    assert_eq!(duplicate.decisions.len(), 1);
    assert_eq!(duplicate.decisions[0].question, "First?");
}

#[test]
fn long_decision_text_is_cut_with_an_ellipsis() {
    let long = "a".repeat(600);
    let text = format!(
        "---\nsaka: spec\n---\n# T\n\n## Acceptance criteria\n\n- **AC-01**: one.\n\n## Done when\n\n- `true`\n\n## Decisions\n\n### {long}\n\n### Q1: Q?\n\n- [x] {long} (recommended)\n- [ ] b\n"
    );
    let parsed = parse("x.md", text.as_bytes());
    let heading = parsed.issues[0].params["heading"].as_str().unwrap();
    assert_eq!(heading.chars().count(), 80);
    assert!(heading.ends_with('…'));
    let decisions = parsed.model.unwrap().decisions;
    let option = &decisions[0].options[0];
    assert_eq!(option.text.chars().count(), 500);
    assert!(option.text.ends_with('…'));
    assert!(option.recommended, "the mark past the cut still counts");
}

#[test]
fn checkboxes_and_level_three_headings_outside_decisions_change_nothing() {
    let parsed = parse("x.md", fixture!("ac-tasklist"));
    assert!(parsed.issues.is_empty(), "{:?}", parsed.issues);
    let model = parsed.model.unwrap();
    let ids: Vec<&str> = model
        .acceptance_criteria
        .iter()
        .map(|a| a.id.as_str())
        .collect();
    assert_eq!(ids, vec!["AC-02"]);
    assert!(model.decisions.is_empty());
}

#[test]
fn specs_without_decisions_have_an_empty_list() {
    for bytes in [
        fixture!("two-step"),
        fixture!("one-step"),
        fixture!("after-graph"),
        fixture!("frontmatter-full"),
    ] {
        let model = parse("x.md", bytes).model.unwrap();
        assert!(model.decisions.is_empty());
        assert_eq!(
            serde_json::to_value(&model).unwrap()["decisions"],
            json!([])
        );
    }
}

#[test]
fn the_rules_list_every_lint_code_once() {
    let rules = rules();
    for code in LintCode::ALL {
        let needle = format!("`{}`", code.as_str());
        assert_eq!(
            rules.matches(&needle).count(),
            1,
            "{} in the rules",
            code.as_str()
        );
    }
    assert_eq!(LintCode::ALL.len(), 46);
    assert!(rules.contains("## Decisions"));
}
