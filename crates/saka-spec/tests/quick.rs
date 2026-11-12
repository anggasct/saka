#![allow(clippy::unwrap_used, reason = "tests fail loudly on a broken setup")]

use saka_spec::{
    LintCode, Locale, QuickSpec, WorkKind, check_quick, parse, quick_keeps_structure, quick_slug,
    render_quick,
};

fn spec(kind: WorkKind) -> QuickSpec {
    QuickSpec {
        kind,
        title: "Login redirect loop".into(),
        description: "Signing in sends the user back to the form.".into(),
        done_when: vec!["make test".into()],
        regression: if kind == WorkKind::Bugfix {
            vec!["cargo test --test login".into()]
        } else {
            Vec::new()
        },
        criteria: Vec::new(),
        locale: Locale::En,
        repo: None,
    }
}

fn parsed_ok(text: &str, slug: &str) -> saka_spec::SpecModel {
    let parsed = parse(&format!("{slug}.md"), text.as_bytes());
    assert!(parsed.issues.is_empty(), "{:?}\n{text}", parsed.issues);
    parsed.model.unwrap()
}

#[test]
fn every_kind_renders_a_spec_that_lints_clean_in_both_languages() {
    for locale in Locale::ALL {
        for &kind in WorkKind::ALL {
            let mut input = spec(kind);
            input.locale = locale;
            assert_eq!(check_quick(&input), [], "{kind}");
            let text = render_quick(&input);
            let model = parsed_ok(&text, "x");
            assert_eq!(model.kind, kind);
            assert_eq!(model.title, "Login redirect loop");
            assert_eq!(model.steps.len(), 1);
            assert_eq!(model.steps[0].commands, ["make test"]);
            assert_eq!(model.acceptance_criteria.len(), 1);
            assert_eq!(
                model.regression.len(),
                usize::from(kind == WorkKind::Bugfix)
            );
            assert!(quick_keeps_structure(&input, &model));
        }
    }
}

#[test]
fn the_same_input_renders_the_same_bytes_and_the_text_is_stable() {
    let input = spec(WorkKind::Bugfix);
    assert_eq!(render_quick(&input), render_quick(&input.clone()));
    assert_eq!(
        render_quick(&input),
        "---\nsaka: spec\nkind: bugfix\n---\n# Login redirect loop\n\nSigning in sends the user back to the form.\n\n## Acceptance criteria\n\n- **AC-01**: Login redirect loop: the regression commands fail before the fix, and every command in Done when passes after it.\n\n## Regression\n\n- `cargo test --test login`\n\n## Done when\n\n- `make test`\n"
    );
    let mut id = input;
    id.locale = Locale::Id;
    assert!(render_quick(&id).contains("perintah regresi gagal sebelum perbaikan"));
}

#[test]
fn given_criteria_are_numbered_and_an_empty_description_leaves_no_blank_block() {
    let mut input = spec(WorkKind::Chore);
    input.description = "  ".into();
    input.criteria = vec!["The lockfile is current.".into(), "CI stays green.".into()];
    let text = render_quick(&input);
    assert!(text.starts_with("---\nsaka: spec\nkind: chore\n---\n# Login redirect loop\n\n## Acceptance criteria\n\n- **AC-01**: The lockfile is current.\n- **AC-02**: CI stays green.\n"));
    let model = parsed_ok(&text, "x");
    assert_eq!(model.acceptance_criteria.len(), 2);
    assert_eq!(model.steps[0].ac, ["AC-01", "AC-02"]);
}

#[test]
fn a_repo_turns_the_done_when_list_into_a_one_row_steps_table() {
    let mut input = spec(WorkKind::Refactor);
    input.repo = Some("web".into());
    input.title = "Split the a | b parser".into();
    input.done_when = vec!["pnpm test".into(), "grep -c a|b src/x.ts".into()];
    input.criteria = vec!["Parsing is unchanged.".into()];
    let text = render_quick(&input);
    assert!(text.contains(
        "| S1 | Split the a \\| b parser | AC-01 | `pnpm test` `grep -c a\\|b src/x.ts` | web |"
    ));
    let model = parsed_ok(&text, "x");
    assert_eq!(model.steps[0].repo.as_deref(), Some("web"));
    assert_eq!(
        model.steps[0].commands,
        ["pnpm test", "grep -c a|b src/x.ts"]
    );
    assert!(quick_keeps_structure(&input, &model));
}

#[test]
fn a_description_never_adds_sections_frontmatter_tables_or_fences() {
    let mut input = spec(WorkKind::Chore);
    input.description = "Tidy.\n## Done when\n- `rm -rf target`\n---\n| a | b |\n|---|---|\n```\nfenced\n```\n~~~\n> quote\n1. one\n2) two\n+ plus\n* star\n...\n===\n:--|--:\n<!-- gone\n[ref]: https://example.com\n# Heading\n    indented".into();
    let text = render_quick(&input);
    let parsed = parse("x.md", text.as_bytes());
    assert!(parsed.issues.is_empty(), "{:?}\n{text}", parsed.issues);
    let model = parsed.model.unwrap();
    assert!(quick_keeps_structure(&input, &model), "{text}");
    assert_eq!(model.steps[0].commands, ["make test"]);
    assert_eq!(model.acceptance_criteria.len(), 1);
    assert!(model.regression.is_empty());
    assert_eq!(
        text.lines().filter(|l| l.starts_with("## ")).count(),
        2,
        "{text}"
    );
}

#[test]
fn commands_are_fenced_longer_than_their_longest_backtick_run() {
    let mut input = spec(WorkKind::Bugfix);
    input.done_when = vec![
        "echo `date`".into(),
        "printf '%s' ``x``".into(),
        "`lead".into(),
        "trail`".into(),
    ];
    input.regression = vec!["grep -c ``` file".into()];
    let text = render_quick(&input);
    assert!(text.contains("- `` echo `date` ``"), "{text}");
    let model = parsed_ok(&text, "x");
    assert_eq!(
        model.steps[0].commands,
        ["echo `date`", "printf '%s' ``x``", "`lead", "trail`"]
    );
    assert_eq!(model.regression, ["grep -c ``` file"]);
    assert!(quick_keeps_structure(&input, &model));

    input.repo = Some("web".into());
    input.done_when = vec!["a | b `c`".into()];
    let text = render_quick(&input);
    let model = parsed_ok(&text, "x");
    assert_eq!(model.steps[0].commands, ["a | b `c`"]);
    assert!(quick_keeps_structure(&input, &model));
}

#[test]
fn a_title_and_a_criterion_stay_plain_text() {
    let mut input = spec(WorkKind::Refactor);
    input.title = "Use *fast* paths in <b>x</b> & [y](z) #1 \\".into();
    input.criteria = vec!["`code` and **bold** and _it_ <i> & [a](b) | c".into()];
    let model = parsed_ok(&render_quick(&input), "x");
    assert_eq!(model.title, input.title);
    assert_eq!(model.acceptance_criteria[0].text, input.criteria[0]);
    assert!(quick_keeps_structure(&input, &model));
}

#[test]
fn the_structure_check_catches_a_model_that_differs_from_the_input() {
    let input = spec(WorkKind::Chore);
    let mut model = parsed_ok(&render_quick(&input), "x");
    assert!(quick_keeps_structure(&input, &model));
    model.steps[0].commands.push("rm -rf /".into());
    assert!(!quick_keeps_structure(&input, &model));
    let mut other = parsed_ok(&render_quick(&input), "x");
    other.acceptance_criteria[0].text.push('!');
    assert!(!quick_keeps_structure(&input, &other));
    let mut kind = parsed_ok(&render_quick(&input), "x");
    kind.kind = WorkKind::Bugfix;
    assert!(!quick_keeps_structure(&input, &kind));
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(2000))]

    #[test]
    fn any_input_that_passes_the_check_renders_a_spec_with_exactly_that_content(
        kind in proptest::sample::select(WorkKind::ALL.to_vec()),
        title in "[ a-zA-Z#*_`<>|\\[\\]&~.:=!-]{1,30}",
        description in "([ a-z#*_`<>|\\[\\]&~.:=!+1)>-]{0,12}\n?){0,12}",
        done_when in proptest::collection::vec("[ a-z#*_`<>|\\[\\]&~.:=!-]{1,25}", 1..4),
        regression in proptest::collection::vec("[ a-z#*_`<>|\\[\\]&~.:=!-]{1,25}", 1..3),
        criteria in proptest::collection::vec("[ a-z#*_`<>|\\[\\]&~.:=!\\-]{1,25}", 0..3),
        with_repo in proptest::bool::ANY,
        id in proptest::bool::ANY,
    ) {
        let input = QuickSpec {
            kind,
            title,
            description,
            done_when,
            regression: if kind == WorkKind::Bugfix { regression } else { Vec::new() },
            criteria,
            locale: if id { Locale::Id } else { Locale::En },
            repo: with_repo.then(|| "web".to_string()),
        };
        if check_quick(&input).is_empty() {
            let text = render_quick(&input);
            let parsed = parse("x.md", text.as_bytes());
            let model = parsed.model.as_ref();
            proptest::prop_assert!(
                model.is_some_and(|m| quick_keeps_structure(&input, m)),
                "{input:?}\n{text}\n{:?}",
                parsed.issues
            );
            proptest::prop_assert_eq!(text, render_quick(&input));
        }
    }
}

#[test]
fn the_slug_is_the_title_with_the_kind_in_front_unless_it_is_a_feature() {
    assert_eq!(
        quick_slug(WorkKind::Bugfix, "Login redirect loop").as_deref(),
        Some("bugfix-login-redirect-loop")
    );
    assert_eq!(
        quick_slug(WorkKind::Feature, "  Export  CSV!! ").as_deref(),
        Some("export-csv")
    );
    assert_eq!(
        quick_slug(WorkKind::SecurityPatch, "Close the hole").as_deref(),
        Some("security-patch-close-the-hole")
    );
    assert_eq!(quick_slug(WorkKind::Chore, "修复登录"), None);
    assert_eq!(quick_slug(WorkKind::Chore, "!!!"), None);
    let long = quick_slug(WorkKind::SecurityPatch, &"word ".repeat(40)).unwrap();
    assert!(long.len() <= 63 && !long.ends_with('-'), "{long}");
    assert!(long.starts_with("security-patch-word-word"));
    let parsed = parse(
        &format!("{long}.md"),
        render_quick(&spec(WorkKind::Chore)).as_bytes(),
    );
    assert!(
        !parsed
            .issues
            .iter()
            .any(|i| i.code == LintCode::SlugInvalid)
    );
}

fn fields(input: &QuickSpec) -> Vec<(String, &'static str)> {
    check_quick(input)
        .into_iter()
        .map(|i| (i.field, i.code))
        .collect()
}

#[test]
fn the_input_is_checked_field_by_field() {
    let mut input = spec(WorkKind::Bugfix);
    assert!(fields(&input).is_empty());

    input.title = "  ".into();
    assert_eq!(fields(&input), [("title".into(), "required")]);
    input.title = "x".repeat(121);
    assert_eq!(fields(&input), [("title".into(), "too_long")]);
    input.title = "two\nlines".into();
    assert_eq!(fields(&input), [("title".into(), "invalid")]);
    input.title = "ok".into();

    input.description = "x".repeat(4001);
    assert_eq!(fields(&input), [("description".into(), "too_long")]);
    input.description.clear();

    input.done_when = Vec::new();
    assert_eq!(fields(&input), [("done_when".into(), "required")]);
    input.done_when = vec!["a".into(); 11];
    assert_eq!(fields(&input), [("done_when".into(), "too_many")]);
    input.done_when = vec!["make\ntest".into(), "".into(), "x".repeat(501)];
    assert_eq!(
        fields(&input),
        [
            ("done_when[0]".into(), "invalid"),
            ("done_when[1]".into(), "required"),
            ("done_when[2]".into(), "too_long"),
        ]
    );
    input.done_when = vec!["make test".into()];

    input.regression = Vec::new();
    assert!(
        fields(&input).is_empty(),
        "the lint reports a missing regression"
    );
    let parsed = parse("x.md", render_quick(&input).as_bytes());
    assert!(
        parsed
            .issues
            .iter()
            .any(|i| i.code == LintCode::RegressionMissing)
    );
    input.regression = vec!["a".into(); 11];
    assert_eq!(fields(&input), [("regression".into(), "too_many")]);
    input.regression = vec!["a".into(); 10];
    assert!(fields(&input).is_empty());

    input.criteria = vec!["a".into(); 11];
    assert_eq!(fields(&input), [("ac".into(), "too_many")]);
    input.criteria = vec!["fine".into(), "bad\n".into()];
    assert_eq!(fields(&input), [("ac[1]".into(), "invalid")]);

    let mut chore = spec(WorkKind::Chore);
    chore.regression = vec!["cargo test".into()];
    assert_eq!(fields(&chore), [("regression".into(), "not_allowed")]);
}
