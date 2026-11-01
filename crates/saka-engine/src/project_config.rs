use serde::Deserialize;
use serde_json::{Value, json};
use toml::Spanned;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectConfig {
    pub name: String,
    pub repos: Vec<RepoEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoEntry {
    pub name: String,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    pub code: &'static str,
    pub field: Option<String>,
    pub line: Option<usize>,
}

impl Issue {
    pub fn to_json(&self) -> Value {
        json!({ "code": self.code, "field": self.field, "line": self.line })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    project: Option<Spanned<RawProject>>,
    repo: Option<Vec<Spanned<RawRepo>>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawProject {
    name: Option<Spanned<String>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRepo {
    name: Option<Spanned<String>>,
    url: Option<Spanned<String>>,
}

fn line_of(text: &str, offset: usize) -> usize {
    text.get(..offset)
        .map_or(1, |s| s.matches('\n').count() + 1)
}

fn issue(code: &'static str, field: &str, text: &str, offset: Option<usize>) -> Issue {
    Issue {
        code,
        field: Some(field.to_string()),
        line: offset.map(|o| line_of(text, o)),
    }
}

fn repo_name_ok(name: &str) -> bool {
    (1..=32).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

pub fn parse(bytes: &[u8]) -> Result<ProjectConfig, Vec<Issue>> {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return Err(vec![Issue {
            code: "toml_invalid",
            field: None,
            line: None,
        }]);
    };
    let raw: Raw = match toml::from_str(text) {
        Ok(raw) => raw,
        Err(e) => {
            let message = e.message().to_string();
            let line = e.span().map(|s| line_of(text, s.start));
            let unknown = message
                .strip_prefix("unknown field `")
                .and_then(|rest| rest.split('`').next())
                .map(String::from);
            let (code, field) = match unknown {
                Some(field) => ("field_unknown", Some(field)),
                None if message.starts_with("invalid type") => ("field_invalid", None),
                None => ("toml_invalid", None),
            };
            return Err(vec![Issue { code, field, line }]);
        }
    };
    let mut issues = Vec::new();
    let name = match raw.project.as_ref().map(|p| (p, p.get_ref().name.as_ref())) {
        None => {
            issues.push(issue("field_missing", "project", text, None));
            None
        }
        Some((p, None)) => {
            issues.push(issue(
                "field_missing",
                "project.name",
                text,
                Some(p.span().start),
            ));
            None
        }
        Some((_, Some(name))) => {
            let len = name.get_ref().chars().count();
            if (1..=80).contains(&len) {
                Some(name.get_ref().clone())
            } else {
                issues.push(issue(
                    "field_invalid",
                    "project.name",
                    text,
                    Some(name.span().start),
                ));
                None
            }
        }
    };
    let mut repos = Vec::new();
    match &raw.repo {
        None => issues.push(issue("field_missing", "repo", text, None)),
        Some(list) if list.is_empty() => issues.push(issue("field_missing", "repo", text, None)),
        Some(list) => {
            for (i, r) in list.iter().enumerate() {
                let at = Some(r.span().start);
                let field = |f: &str| format!("repo[{i}].{f}");
                let entry = r.get_ref();
                let name = match &entry.name {
                    None => {
                        issues.push(issue("field_missing", &field("name"), text, at));
                        None
                    }
                    Some(n) if !repo_name_ok(n.get_ref()) => {
                        issues.push(issue(
                            "field_invalid",
                            &field("name"),
                            text,
                            Some(n.span().start),
                        ));
                        None
                    }
                    Some(n) => Some(n.get_ref().clone()),
                };
                let url = match &entry.url {
                    None => {
                        issues.push(issue("field_missing", &field("url"), text, at));
                        None
                    }
                    Some(u) if crate::register::parse_repo_url(u.get_ref()).is_none() => {
                        issues.push(issue(
                            "field_invalid",
                            &field("url"),
                            text,
                            Some(u.span().start),
                        ));
                        None
                    }
                    Some(u) => Some(u.get_ref().clone()),
                };
                if let (Some(name), Some(url)) = (name, url) {
                    repos.push(RepoEntry { name, url });
                }
            }
        }
    }
    match name {
        Some(name) if issues.is_empty() => Ok(ProjectConfig { name, repos }),
        _ => Err(issues),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn codes(text: &str) -> Vec<(&'static str, Option<String>, Option<usize>)> {
        parse(text.as_bytes())
            .unwrap_err()
            .into_iter()
            .map(|i| (i.code, i.field, i.line))
            .collect()
    }

    #[test]
    fn a_valid_file_parses() {
        let cfg = parse(
            b"[project]\nname = \"tasks\"\n\n[[repo]]\nname = \"app\"\nurl = \"https://github.com/acme/app\"\n",
        )
        .unwrap();
        assert_eq!(cfg.name, "tasks");
        assert_eq!(
            cfg.repos,
            [RepoEntry {
                name: "app".into(),
                url: "https://github.com/acme/app".into()
            }]
        );
    }

    #[test]
    fn problems_carry_a_code_field_and_line() {
        assert_eq!(
            codes("[project]\nname = \"x\"\ncolor = \"red\"\n"),
            [("field_unknown", Some("color".into()), Some(3))]
        );
        assert_eq!(codes("[project\n"), [("toml_invalid", None, Some(1))]);
        assert_eq!(
            codes("[project]\nname = \"x\"\n"),
            [("field_missing", Some("repo".into()), None)]
        );
        assert_eq!(
            codes("[project]\n\n[[repo]]\nname = \"App!\"\nurl = \"ftp://x\"\n"),
            [
                ("field_missing", Some("project.name".into()), Some(1)),
                ("field_invalid", Some("repo[0].name".into()), Some(4)),
                ("field_invalid", Some("repo[0].url".into()), Some(5)),
            ]
        );
    }
}
