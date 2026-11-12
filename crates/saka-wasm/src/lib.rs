use saka_spec::{
    LintCode, Locale, PARSER_VERSION, QuickSpec, WorkKind, check_quick, quick_keeps_structure,
    quick_slug, render_quick,
};
use serde_json::{Value, json};
use wasm_bindgen::prelude::wasm_bindgen;

fn report(file_name: &str, text: &str) -> Value {
    if !saka_spec::is_spec(text) {
        return json!({
            "is_spec": false,
            "parser_version": PARSER_VERSION,
            "issues": [],
            "steps": [],
        });
    }
    let parsed = saka_spec::parse(file_name, text.as_bytes());
    let steps: Vec<Value> = parsed
        .model
        .iter()
        .flat_map(|m| &m.steps)
        .map(|s| json!({ "id": s.id, "line": s.line }))
        .collect();
    json!({
        "is_spec": true,
        "parser_version": PARSER_VERSION,
        "issues": parsed.issues,
        "steps": steps,
    })
}

#[wasm_bindgen]
pub fn lint(file_name: &str, text: &str) -> String {
    report(file_name, text).to_string()
}

fn strings(value: &Value, key: &str) -> Vec<String> {
    value[key]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| item.as_str().map(str::to_string))
        .collect()
}

fn quick_input(value: &Value) -> Option<QuickSpec> {
    Some(QuickSpec {
        kind: value["kind"].as_str()?.parse::<WorkKind>().ok()?,
        title: value["title"].as_str()?.to_string(),
        description: value["description"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        done_when: strings(value, "done_when"),
        regression: strings(value, "regression"),
        criteria: strings(value, "ac"),
        locale: value["locale"]
            .as_str()
            .and_then(Locale::from_code)
            .unwrap_or_default(),
        repo: value["repo"]
            .as_str()
            .filter(|r| !r.is_empty())
            .map(str::to_string),
    })
}

fn quick_report(input: &str) -> Value {
    let Some(spec) = serde_json::from_str::<Value>(input)
        .ok()
        .as_ref()
        .and_then(quick_input)
    else {
        return json!({ "valid": false, "issues": [], "text": "", "slug": null, "lint": [] });
    };
    let issues: Vec<Value> = check_quick(&spec)
        .into_iter()
        .map(|i| json!({ "field": i.field, "code": i.code }))
        .collect();
    let text = render_quick(&spec);
    let slug = quick_slug(spec.kind, &spec.title);
    let file = format!("{}.md", slug.as_deref().unwrap_or("quick"));
    let parsed = saka_spec::parse(&file, text.as_bytes());
    let structure = parsed
        .model
        .as_ref()
        .is_some_and(|model| quick_keeps_structure(&spec, model));
    json!({
        "valid": true,
        "issues": issues,
        "structure": structure,
        "text": text,
        "slug": slug,
        "lint": parsed.issues,
    })
}

#[wasm_bindgen]
pub fn quick_preview(input: &str) -> String {
    quick_report(input).to_string()
}

#[wasm_bindgen]
pub fn content_hash(text: &str) -> String {
    saka_spec::content_hash(text.as_bytes())
}

#[wasm_bindgen]
pub fn lint_codes() -> String {
    let codes: Vec<Value> = LintCode::ALL
        .iter()
        .map(|c| json!({ "code": c.as_str(), "severity": c.severity().as_str() }))
        .collect();
    Value::Array(codes).to_string()
}

#[cfg(test)]
mod tests {
    use saka_spec::{Locale, WorkKind, template};

    use super::*;

    fn lint_json(file_name: &str, text: &str) -> Value {
        serde_json::from_str(&lint(file_name, text)).unwrap()
    }

    #[test]
    fn every_template_lints_clean_with_its_steps_and_lines() {
        for locale in [Locale::En, Locale::Id] {
            for &kind in WorkKind::ALL {
                let text = template(locale, kind);
                let report = lint_json("example-spec.md", text);
                assert_eq!(report["is_spec"], true);
                assert_eq!(report["parser_version"], PARSER_VERSION);
                assert_eq!(report["issues"], json!([]), "{locale:?} {kind}");
                let steps = report["steps"].as_array().unwrap();
                let ids: Vec<&str> = steps.iter().map(|s| s["id"].as_str().unwrap()).collect();
                if kind == WorkKind::Feature {
                    assert_eq!(ids, ["S1", "S2"]);
                    for step in steps {
                        let line = step["line"].as_u64().unwrap() as usize;
                        let row = text.lines().nth(line - 1).unwrap();
                        assert!(
                            row.trim_start().starts_with('|')
                                && row.contains(step["id"].as_str().unwrap()),
                            "{row}"
                        );
                    }
                } else {
                    assert_eq!(ids, ["S1"]);
                    let line = steps[0]["line"].as_u64().unwrap() as usize;
                    assert_eq!(text.lines().nth(line - 1), Some("## Done when"));
                }
            }
        }
    }

    #[test]
    fn kind_and_regression_issues_come_back_from_the_browser_lint() {
        let bugfix = template(Locale::En, WorkKind::Bugfix);
        let without = bugfix.replace("## Regression", "## Notes about the test");
        let report = lint_json("fix.md", &without);
        let codes: Vec<&str> = report["issues"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["code"].as_str().unwrap())
            .collect();
        assert_eq!(codes, ["regression_missing"]);
        let chore = template(Locale::En, WorkKind::Chore)
            .replace("## Notes", "## Regression\n\n- `make test`\n\n## Notes");
        let report = lint_json("tidy.md", &chore);
        assert_eq!(report["issues"][0]["code"], "regression_ignored");
        assert_eq!(report["issues"][0]["severity"], "warning");
        let unknown = template(Locale::En, WorkKind::Chore).replace("kind: chore", "kind: hotfix");
        let report = lint_json("tidy.md", &unknown);
        assert_eq!(report["issues"][0]["code"], "kind_invalid");
        assert_eq!(report["issues"][0]["params"]["value"], "hotfix");
    }

    #[test]
    fn issues_are_exactly_the_ones_the_server_parser_reports() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../saka-spec/tests/fixtures");
        let mut seen = 0;
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_str().unwrap().to_string();
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            if !saka_spec::is_spec(&text) {
                continue;
            }
            seen += 1;
            let expected = saka_spec::parse(&name, text.as_bytes());
            let report = lint_json(&name, &text);
            assert_eq!(
                report["issues"],
                serde_json::to_value(&expected.issues).unwrap(),
                "{name}"
            );
            let steps = expected.model.map(|m| m.steps.len()).unwrap_or_default();
            assert_eq!(report["steps"].as_array().unwrap().len(), steps, "{name}");
        }
        assert!(seen > 10, "only {seen} fixtures");
    }

    #[test]
    fn open_decisions_come_back_as_their_lint_code() {
        let text = include_str!("../../saka-spec/tests/fixtures/decisions-ok.md");
        let report = lint_json("docs/specs/uploads.md", text);
        assert_eq!(
            report["issues"],
            json!([{
                "code": "decision_unanswered",
                "severity": "error",
                "line": 29,
                "params": { "id": "Q2" },
            }])
        );
    }

    #[test]
    fn a_plain_doc_is_not_a_spec_and_has_no_issues() {
        let report = lint_json("notes.md", "# Notes\n\nNothing to lint.\n");
        assert_eq!(
            report,
            json!({ "is_spec": false, "parser_version": PARSER_VERSION, "issues": [], "steps": [] })
        );
    }

    #[test]
    fn the_quick_preview_is_the_server_render_with_its_field_issues() {
        let input = json!({
            "kind": "bugfix",
            "title": "Login redirect loop",
            "description": "Back to the form.",
            "done_when": ["make test"],
            "regression": ["cargo test --test login"],
            "ac": [],
            "locale": "id",
            "repo": "",
        });
        let report: Value = serde_json::from_str(&quick_preview(&input.to_string())).unwrap();
        let spec = quick_input(&input).unwrap();
        assert_eq!(report["valid"], true);
        assert_eq!(report["text"], render_quick(&spec));
        assert_eq!(report["slug"], "bugfix-login-redirect-loop");
        assert_eq!(report["issues"], json!([]));
        assert_eq!(report["lint"], json!([]));
        assert_eq!(report["structure"], true);

        let mut bad = input.clone();
        bad["title"] = json!("");
        bad["done_when"] = json!([]);
        bad["regression"] = json!([]);
        let report: Value = serde_json::from_str(&quick_preview(&bad.to_string())).unwrap();
        assert_eq!(
            report["issues"],
            json!([
                { "field": "title", "code": "required" },
                { "field": "done_when", "code": "required" },
            ])
        );
        assert_eq!(report["slug"], Value::Null);
        let codes: Vec<&str> = report["lint"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["code"].as_str().unwrap())
            .collect();
        assert!(codes.contains(&"regression_missing"), "{codes:?}");

        let unknown: Value = serde_json::from_str(&quick_preview("{\"kind\":\"hotfix\"}")).unwrap();
        assert_eq!(unknown["valid"], false);
        let garbage: Value = serde_json::from_str(&quick_preview("not json")).unwrap();
        assert_eq!(garbage["valid"], false);
    }

    #[test]
    fn the_hash_matches_the_server_hash() {
        let text = "\u{feff}# A\r\nline\r\n";
        assert_eq!(content_hash(text), saka_spec::content_hash(text.as_bytes()));
    }

    #[test]
    fn every_lint_code_is_listed_with_its_severity() {
        let codes: Value = serde_json::from_str(&lint_codes()).unwrap();
        let codes = codes.as_array().unwrap();
        assert_eq!(codes.len(), LintCode::ALL.len());
        for (listed, code) in codes.iter().zip(LintCode::ALL) {
            assert_eq!(listed["code"], code.as_str());
            assert_eq!(listed["severity"], code.severity().as_str());
        }
    }
}
