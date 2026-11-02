use saka_spec::{LintCode, PARSER_VERSION};
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
    use saka_spec::{Locale, template};

    use super::*;

    fn lint_json(file_name: &str, text: &str) -> Value {
        serde_json::from_str(&lint(file_name, text)).unwrap()
    }

    #[test]
    fn both_templates_lint_clean_with_their_steps_and_lines() {
        for locale in [Locale::En, Locale::Id] {
            let text = template(locale);
            let report = lint_json("example-spec.md", text);
            assert_eq!(report["is_spec"], true);
            assert_eq!(report["parser_version"], PARSER_VERSION);
            assert_eq!(report["issues"], json!([]), "{locale:?}");
            let steps = report["steps"].as_array().unwrap();
            let ids: Vec<&str> = steps.iter().map(|s| s["id"].as_str().unwrap()).collect();
            assert_eq!(ids, ["S1", "S2"]);
            for step in steps {
                let line = step["line"].as_u64().unwrap() as usize;
                let row = text.lines().nth(line - 1).unwrap();
                assert!(
                    row.trim_start().starts_with('|') && row.contains(step["id"].as_str().unwrap()),
                    "{row}"
                );
            }
        }
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
    fn a_plain_doc_is_not_a_spec_and_has_no_issues() {
        let report = lint_json("notes.md", "# Notes\n\nNothing to lint.\n");
        assert_eq!(
            report,
            json!({ "is_spec": false, "parser_version": PARSER_VERSION, "issues": [], "steps": [] })
        );
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
