use std::collections::BTreeMap;

use saka_core::scope::{PathScope, ScopeIssue};
use saka_core::vocab::Autonomy;
use saka_core::workflow::Workflow;
use saka_core::workspace::MAX_BINDINGS;
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use toml::Spanned;
use toml::de::{DeTable, DeValue};

mod workflow;

use crate::register::{branch_ok, parse_repo_url};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectConfig {
    pub name: String,
    pub workflow: Workflow,
    pub hook_skip: Vec<HookSkip>,
    pub repos: Vec<RepoEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookSkip {
    pub name: String,
    pub index: usize,
    pub line: usize,
}

pub const MAX_HOOK_SKIP: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoEntry {
    pub index: usize,
    pub name: String,
    pub url: String,
    pub host: String,
    pub full_name: String,
    pub is_default: bool,
    pub base_branch: Option<String>,
    pub path_scope: Vec<String>,
    pub autonomy: Option<Autonomy>,
    pub line: usize,
    pub base_branch_line: Option<usize>,
}

impl RepoEntry {
    pub fn same_repo(&self, host: &str, full_name: &str) -> bool {
        self.host == host && self.full_name.eq_ignore_ascii_case(full_name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Issue {
    pub code: &'static str,
    pub field: Option<String>,
    pub line: Option<usize>,
}

impl Issue {
    pub fn to_json(&self) -> Value {
        json!({ "code": self.code, "field": self.field, "line": self.line })
    }

    pub fn file_missing() -> Issue {
        Issue {
            code: "file_missing",
            field: None,
            line: None,
        }
    }
}

pub fn issues_json(issues: &[Issue]) -> Value {
    issues.iter().map(Issue::to_json).collect()
}

pub fn sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    format!("sha256:{hex}")
}

const REPO_KEYS: &[&str] = &[
    "name",
    "url",
    "default",
    "base_branch",
    "path_scope",
    "autonomy",
];

fn line_of(text: &str, offset: usize) -> usize {
    text.get(..offset)
        .map_or(1, |s| s.matches('\n').count() + 1)
}

struct Collector<'t> {
    text: &'t str,
    issues: Vec<Issue>,
}

impl Collector<'_> {
    fn push(&mut self, code: &'static str, field: &str, at: Option<usize>) {
        self.issues.push(Issue {
            code,
            field: Some(field.to_string()),
            line: at.map(|o| line_of(self.text, o)),
        });
    }

    fn unknown(&mut self, table: &DeTable<'_>, prefix: &str, known: &[&str]) {
        let mut keys: Vec<_> = table
            .iter()
            .filter(|(k, _)| !known.contains(&k.get_ref().as_ref()))
            .collect();
        keys.sort_by_key(|(k, _)| k.span().start);
        for (k, _) in keys {
            let field = format!("{prefix}{}", k.get_ref());
            self.push("field_unknown", &field, Some(k.span().start));
        }
    }
}

fn get<'a, 'i>(table: &'a DeTable<'i>, key: &str) -> Option<&'a Spanned<DeValue<'i>>> {
    table
        .iter()
        .find(|(k, _)| k.get_ref().as_ref() == key)
        .map(|(_, v)| v)
}

fn string(
    c: &mut Collector<'_>,
    table: &DeTable<'_>,
    key: &str,
    field: &str,
    at: usize,
) -> Option<(String, usize)> {
    match get(table, key) {
        None => {
            c.push("field_missing", field, Some(at));
            None
        }
        Some(v) => match v.get_ref().as_str() {
            Some(s) => Some((s.to_string(), v.span().start)),
            None => {
                c.push("field_invalid", field, Some(v.span().start));
                None
            }
        },
    }
}

fn autonomy(c: &mut Collector<'_>, table: &DeTable<'_>, field: &str) -> Option<Autonomy> {
    let v = get(table, "autonomy")?;
    let parsed = v
        .get_ref()
        .as_str()
        .and_then(|s| s.parse::<Autonomy>().ok());
    if parsed.is_none() {
        c.push("field_invalid", field, Some(v.span().start));
    }
    parsed
}

fn repo_name_ok(name: &str) -> bool {
    (1..=32).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

struct Raw {
    entry: RepoEntry,
    default: Option<(bool, usize)>,
    name_at: usize,
    url_at: usize,
}

fn repo(c: &mut Collector<'_>, i: usize, value: &Spanned<DeValue<'_>>) -> Option<Raw> {
    let at = value.span().start;
    let field = |f: &str| format!("repo[{i}].{f}");
    let Some(table) = value.get_ref().as_table() else {
        c.push("field_invalid", &format!("repo[{i}]"), Some(at));
        return None;
    };
    c.unknown(table, &format!("repo[{i}]."), REPO_KEYS);
    let name = string(c, table, "name", &field("name"), at).and_then(|(n, n_at)| {
        if repo_name_ok(&n) {
            Some((n, n_at))
        } else {
            c.push("field_invalid", &field("name"), Some(n_at));
            None
        }
    });
    let url =
        string(c, table, "url", &field("url"), at).and_then(|(u, u_at)| match parse_repo_url(&u) {
            Some((host, owner, repo)) => Some((u, host, format!("{owner}/{repo}"), u_at)),
            None => {
                c.push("field_invalid", &field("url"), Some(u_at));
                None
            }
        });
    let default = match get(table, "default") {
        None => None,
        Some(v) => match v.get_ref().as_bool() {
            Some(b) => Some((b, v.span().start)),
            None => {
                c.push("field_invalid", &field("default"), Some(v.span().start));
                None
            }
        },
    };
    let base_branch = match get(table, "base_branch") {
        None => None,
        Some(v) => match v.get_ref().as_str().filter(|b| branch_ok(b)) {
            Some(b) => Some((b.to_string(), v.span().start)),
            None => {
                c.push("field_invalid", &field("base_branch"), Some(v.span().start));
                None
            }
        },
    };
    let path_scope = match get(table, "path_scope") {
        None => Some(vec![saka_core::scope::EVERYTHING.to_string()]),
        Some(v) => scope(c, &field("path_scope"), v),
    };
    let requested = autonomy(c, table, &field("autonomy"));
    let ((name, name_at), (url, host, full_name, url_at), path_scope) = (name?, url?, path_scope?);
    Some(Raw {
        entry: RepoEntry {
            index: i,
            name,
            url,
            host,
            full_name,
            is_default: false,
            base_branch_line: base_branch.as_ref().map(|(_, b)| line_of(c.text, *b)),
            base_branch: base_branch.map(|(b, _)| b),
            path_scope,
            autonomy: requested,
            line: line_of(c.text, at),
        },
        default,
        name_at,
        url_at,
    })
}

fn scope(c: &mut Collector<'_>, field: &str, value: &Spanned<DeValue<'_>>) -> Option<Vec<String>> {
    let at = value.span().start;
    let Some(items) = value.get_ref().as_array() else {
        c.push("field_invalid", field, Some(at));
        return None;
    };
    let mut patterns = Vec::new();
    let mut lines = Vec::new();
    for item in items.iter() {
        let Some(p) = item.get_ref().as_str() else {
            c.push("field_invalid", field, Some(item.span().start));
            return None;
        };
        patterns.push(p.to_string());
        lines.push(item.span().start);
    }
    match PathScope::parse(&patterns) {
        Ok(_) => Some(patterns),
        Err(issues) => {
            for issue in issues {
                match issue {
                    ScopeIssue::TooMany => c.push("field_invalid", field, Some(at)),
                    ScopeIssue::Pattern(j) => c.push(
                        "path_scope_invalid",
                        &format!("{field}[{j}]"),
                        lines.get(j).copied(),
                    ),
                    ScopeIssue::NoInclude => c.push("path_scope_empty", field, Some(at)),
                }
            }
            None
        }
    }
}

fn project_name(c: &mut Collector<'_>, doc: &DeTable<'_>) -> Option<(String, Option<Autonomy>)> {
    let Some(project) = get(doc, "project") else {
        c.issues.push(Issue {
            code: "field_missing",
            field: Some("project".into()),
            line: None,
        });
        return None;
    };
    let at = project.span().start;
    let Some(table) = project.get_ref().as_table() else {
        c.push("field_invalid", "project", Some(at));
        return None;
    };
    c.unknown(table, "project.", &["name", "autonomy"]);
    let requested = autonomy(c, table, "project.autonomy");
    let (name, name_at) = string(c, table, "name", "project.name", at)?;
    if (1..=80).contains(&name.chars().count()) {
        Some((name, requested))
    } else {
        c.push("field_invalid", "project.name", Some(name_at));
        None
    }
}

fn hook_skip(c: &mut Collector<'_>, doc: &DeTable<'_>) -> Vec<HookSkip> {
    let Some(section) = get(doc, "hooks") else {
        return Vec::new();
    };
    let Some(table) = section.get_ref().as_table() else {
        c.push("field_invalid", "hooks", Some(section.span().start));
        return Vec::new();
    };
    c.unknown(table, "hooks.", &["skip"]);
    let Some(list) = get(table, "skip") else {
        return Vec::new();
    };
    let Some(items) = list.get_ref().as_array() else {
        c.push("field_invalid", "hooks.skip", Some(list.span().start));
        return Vec::new();
    };
    if items.len() > MAX_HOOK_SKIP {
        c.push("field_invalid", "hooks.skip", Some(list.span().start));
        return Vec::new();
    }
    let mut out: Vec<HookSkip> = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let at = item.span().start;
        let field = format!("hooks.skip[{index}]");
        match item
            .get_ref()
            .as_str()
            .filter(|n| saka_core::hook::slug_ok(n))
        {
            Some(name) if out.iter().any(|s| s.name == name) => {
                c.push("field_invalid", &field, Some(at))
            }
            Some(name) => out.push(HookSkip {
                name: name.to_string(),
                index,
                line: line_of(c.text, at),
            }),
            None => c.push("field_invalid", &field, Some(at)),
        }
    }
    out
}

fn repos(c: &mut Collector<'_>, doc: &DeTable<'_>) -> Vec<RepoEntry> {
    let list = match get(doc, "repo") {
        None => None,
        Some(v) => match v.get_ref().as_array() {
            Some(items) => Some(items),
            None => {
                c.push("field_invalid", "repo", Some(v.span().start));
                return Vec::new();
            }
        },
    };
    let Some(list) = list.filter(|l| !l.is_empty()) else {
        c.issues.push(Issue {
            code: "field_missing",
            field: Some("repo".into()),
            line: None,
        });
        return Vec::new();
    };
    if let Some(extra) = list.get(MAX_BINDINGS) {
        c.push("too_many_repos", "repo", Some(extra.span().start));
    }
    let raws: Vec<Raw> = list
        .iter()
        .enumerate()
        .filter_map(|(i, v)| repo(c, i, v))
        .collect();
    let mut names = BTreeMap::new();
    let mut urls = BTreeMap::new();
    for raw in &raws {
        let e = &raw.entry;
        if names.insert(e.name.clone(), e.index).is_some() {
            c.push(
                "repo_name_duplicate",
                &format!("repo[{}].name", e.index),
                Some(raw.name_at),
            );
        }
        let key = (e.host.clone(), e.full_name.to_ascii_lowercase());
        if urls.insert(key, e.index).is_some() {
            c.push(
                "repo_url_duplicate",
                &format!("repo[{}].url", e.index),
                Some(raw.url_at),
            );
        }
    }
    let defaults = |raw: &&Raw| raw.default.is_some_and(|(d, _)| d);
    let default_at = |raw: &Raw| raw.default.map(|(_, at)| at);
    if list.len() == 1 {
        if let Some(raw) = raws.first().filter(|r| r.default.is_some_and(|(d, _)| !d)) {
            c.push("default_missing", "repo[0].default", default_at(raw));
        }
    } else {
        let chosen: Vec<&Raw> = raws.iter().filter(defaults).collect();
        let declared = list.iter().filter(|v| {
            v.get_ref()
                .as_table()
                .and_then(|t| get(t, "default"))
                .is_some_and(|d| d.get_ref().as_bool() == Some(true))
        });
        if declared.count() == 0 {
            c.issues.push(Issue {
                code: "default_missing",
                field: Some("repo".into()),
                line: None,
            });
        }
        for raw in chosen.iter().skip(1) {
            c.push(
                "default_duplicate",
                &format!("repo[{}].default", raw.entry.index),
                default_at(raw),
            );
        }
    }
    let single = list.len() == 1;
    raws.into_iter()
        .map(|raw| {
            let is_default = single || raw.default.is_some_and(|(d, _)| d);
            RepoEntry {
                is_default,
                ..raw.entry
            }
        })
        .collect()
}

pub fn parse(bytes: &[u8]) -> Result<ProjectConfig, Vec<Issue>> {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return Err(vec![Issue {
            code: "toml_invalid",
            field: None,
            line: None,
        }]);
    };
    let doc = match DeTable::parse(text) {
        Ok(doc) => doc,
        Err(e) => {
            return Err(vec![Issue {
                code: "toml_invalid",
                field: None,
                line: e.span().map(|s| line_of(text, s.start)),
            }]);
        }
    };
    let doc = doc.get_ref();
    let mut c = Collector {
        text,
        issues: Vec::new(),
    };
    c.unknown(doc, "", &["project", "repo", "workflow", "hooks"]);
    let (name, project_autonomy) = match project_name(&mut c, doc) {
        Some((name, requested)) => (Some(name), requested),
        None => (None, None),
    };
    let mut repos = repos(&mut c, doc);
    for repo in &mut repos {
        repo.autonomy = repo.autonomy.or(project_autonomy);
    }
    let workflow = workflow::parse(&mut c, doc);
    let hook_skip = hook_skip(&mut c, doc);
    let mut issues = c.issues;
    issues.sort_by_key(|i| i.line.unwrap_or(0));
    match name {
        Some(name) if issues.is_empty() => Ok(ProjectConfig {
            name,
            workflow,
            hook_skip,
            repos,
        }),
        _ => Err(issues),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type Row = (&'static str, Option<String>, Option<usize>);

    fn codes(text: &str) -> Vec<Row> {
        parse(text.as_bytes())
            .unwrap_err()
            .into_iter()
            .map(|i| (i.code, i.field, i.line))
            .collect()
    }

    fn row(code: &'static str, field: &str, line: Option<usize>) -> Row {
        (code, Some(field.to_string()), line)
    }

    const TWO_REPOS: &str = r#"[project]
name = "shop"

[[repo]]
name = "api"
url = "https://github.com/acme/shop"
default = true
base_branch = "main"
path_scope = ["services/api/**", "pnpm-lock.yaml", "!services/api/fixtures/**"]

[[repo]]
name = "web"
url = "https://github.com/acme/web"
"#;

    #[test]
    fn two_repos_parse_with_their_default_branch_and_scope() {
        let cfg = parse(TWO_REPOS.as_bytes()).unwrap();
        assert_eq!(cfg.name, "shop");
        let [api, web] = cfg.repos.as_slice() else {
            panic!("two repos: {:?}", cfg.repos)
        };
        assert_eq!((api.name.as_str(), api.is_default), ("api", true));
        assert_eq!(api.base_branch.as_deref(), Some("main"));
        assert_eq!(api.base_branch_line, Some(8));
        assert_eq!(
            api.path_scope,
            [
                "services/api/**",
                "pnpm-lock.yaml",
                "!services/api/fixtures/**"
            ]
        );
        assert_eq!(
            (api.host.as_str(), api.full_name.as_str()),
            ("github.com", "acme/shop")
        );
        assert_eq!((web.name.as_str(), web.is_default), ("web", false));
        assert_eq!(web.base_branch, None);
        assert_eq!(web.path_scope, ["**"]);
        assert_eq!(web.line, 11);
    }

    #[test]
    fn a_repo_asks_for_its_own_autonomy_or_takes_the_project_one() {
        let text = TWO_REPOS
            .replace(
                "name = \"shop\"\n",
                "name = \"shop\"\nautonomy = \"strict\"\n",
            )
            .replace(
                "url = \"https://github.com/acme/web\"\n",
                "url = \"https://github.com/acme/web\"\nautonomy = \"autonomous\"\n",
            );
        let cfg = parse(text.as_bytes()).unwrap();
        let levels: Vec<_> = cfg.repos.iter().map(|r| r.autonomy).collect();
        assert_eq!(
            levels,
            [Some(Autonomy::Strict), Some(Autonomy::Autonomous)],
            "the project level is the default, a repo overrides it"
        );
        let none = parse(TWO_REPOS.as_bytes()).unwrap();
        assert!(none.repos.iter().all(|r| r.autonomy.is_none()));
    }

    #[test]
    fn an_unknown_autonomy_is_a_problem_at_its_line() {
        assert_eq!(
            codes(
                "[project]\nname = \"x\"\nautonomy = \"reckless\"\n\n[[repo]]\nname = \"a\"\nurl = \"https://github.com/acme/a\"\nautonomy = 3\n"
            ),
            [
                row("field_invalid", "project.autonomy", Some(3)),
                row("field_invalid", "repo[0].autonomy", Some(8)),
            ]
        );
    }

    #[test]
    fn hooks_to_skip_are_named_once_each_and_nothing_else_is_taken() {
        let text = format!("{TWO_REPOS}\n[hooks]\nskip = [\"announce\", \"audit\"]\n");
        let cfg = parse(text.as_bytes()).unwrap();
        let names: Vec<_> = cfg
            .hook_skip
            .iter()
            .map(|s| (s.name.as_str(), s.line))
            .collect();
        assert_eq!(names, [("announce", 16), ("audit", 16)]);
        assert!(parse(TWO_REPOS.as_bytes()).unwrap().hook_skip.is_empty());
        assert_eq!(
            codes(&format!(
                "{TWO_REPOS}\n[hooks]\nskip = [\"a\", \"a\", \"Bad Name\"]\nrun = []\n"
            ))
            .iter()
            .map(|(c, f, _)| (*c, f.clone().unwrap()))
            .collect::<Vec<_>>(),
            [
                ("field_invalid", "hooks.skip[1]".to_string()),
                ("field_invalid", "hooks.skip[2]".to_string()),
                ("field_unknown", "hooks.run".to_string()),
            ]
        );
    }

    #[test]
    fn a_single_repo_is_the_default_without_saying_so() {
        let cfg = parse(
            b"[project]\nname = \"tasks\"\n\n[[repo]]\nname = \"app\"\nurl = \"https://github.com/acme/app\"\n",
        )
        .unwrap();
        assert_eq!(cfg.name, "tasks");
        assert_eq!(cfg.repos.len(), 1);
        assert!(cfg.repos[0].is_default);
        assert_eq!(cfg.repos[0].path_scope, ["**"]);
    }

    #[test]
    fn problems_carry_a_code_field_and_line() {
        assert_eq!(
            codes("[project]\nname = \"x\"\ncolor = \"red\"\n"),
            [
                row("field_missing", "repo", None),
                row("field_unknown", "project.color", Some(3)),
            ]
        );
        assert_eq!(codes("[project\n"), [("toml_invalid", None, Some(1))]);
        assert_eq!(
            codes("[project]\n\n[[repo]]\nname = \"App!\"\nurl = \"ftp://x\"\n"),
            [
                row("field_missing", "project.name", Some(1)),
                row("field_invalid", "repo[0].name", Some(4)),
                row("field_invalid", "repo[0].url", Some(5)),
            ]
        );
    }

    fn repo_block(name: &str, extra: &str) -> String {
        format!("[[repo]]\nname = \"{name}\"\nurl = \"https://github.com/acme/{name}\"\n{extra}")
    }

    #[test]
    fn more_than_sixteen_repos_are_refused_at_the_seventeenth() {
        let mut text = String::from("[project]\nname = \"big\"\n");
        for n in 0..17 {
            let extra = if n == 0 { "default = true\n" } else { "" };
            text.push_str(&repo_block(&format!("r{n}"), extra));
        }
        let line = 3 + 16 * 3 + 1;
        assert_eq!(codes(&text), [row("too_many_repos", "repo", Some(line))]);
    }

    #[test]
    fn names_and_urls_are_unique_after_normalizing() {
        let text = "[project]\nname = \"x\"\n[[repo]]\nname = \"a\"\nurl = \"https://github.com/acme/a\"\ndefault = true\n[[repo]]\nname = \"a\"\nurl = \"https://GitHub.com/Acme/A.git/\"\n";
        assert_eq!(
            codes(text),
            [
                row("repo_name_duplicate", "repo[1].name", Some(8)),
                row("repo_url_duplicate", "repo[1].url", Some(9)),
            ]
        );
    }

    #[test]
    fn exactly_one_repo_is_the_default() {
        let one = "[project]\nname = \"x\"\n[[repo]]\nname = \"a\"\nurl = \"https://github.com/acme/a\"\ndefault = false\n";
        assert_eq!(
            codes(one),
            [row("default_missing", "repo[0].default", Some(6))]
        );
        let none = format!(
            "[project]\nname = \"x\"\n{}{}",
            repo_block("a", ""),
            repo_block("b", "")
        );
        assert_eq!(codes(&none), [row("default_missing", "repo", None)]);
        let two = format!(
            "[project]\nname = \"x\"\n{}{}{}",
            repo_block("a", "default = true\n"),
            repo_block("b", "default = true\n"),
            repo_block("c", "default = true\n"),
        );
        assert_eq!(
            codes(&two),
            [
                row("default_duplicate", "repo[1].default", Some(10)),
                row("default_duplicate", "repo[2].default", Some(14)),
            ]
        );
        let typed = "[project]\nname = \"x\"\n[[repo]]\nname = \"a\"\nurl = \"https://github.com/acme/a\"\ndefault = \"yes\"\n";
        assert_eq!(
            codes(typed),
            [row("field_invalid", "repo[0].default", Some(6))]
        );
    }

    #[test]
    fn scope_patterns_are_checked_one_by_one() {
        let text = "[project]\nname = \"x\"\n[[repo]]\nname = \"a\"\nurl = \"https://github.com/acme/a\"\npath_scope = [\n  \"/a\",\n  \"a\\\\b\",\n  \"a/../b\",\n  \"[\",\n  \"!\",\n  \"ok/**\",\n]\n";
        assert_eq!(
            codes(text),
            [
                row("path_scope_invalid", "repo[0].path_scope[0]", Some(7)),
                row("path_scope_invalid", "repo[0].path_scope[1]", Some(8)),
                row("path_scope_invalid", "repo[0].path_scope[2]", Some(9)),
                row("path_scope_invalid", "repo[0].path_scope[3]", Some(10)),
                row("path_scope_invalid", "repo[0].path_scope[4]", Some(11)),
            ]
        );
        let empty = "[project]\nname = \"x\"\n[[repo]]\nname = \"a\"\nurl = \"https://github.com/acme/a\"\npath_scope = [\"!a/**\"]\n";
        assert_eq!(
            codes(empty),
            [row("path_scope_empty", "repo[0].path_scope", Some(6))]
        );
        let typed = "[project]\nname = \"x\"\n[[repo]]\nname = \"a\"\nurl = \"https://github.com/acme/a\"\npath_scope = \"a/**\"\nbase_branch = \"a..b\"\n";
        assert_eq!(
            codes(typed),
            [
                row("field_invalid", "repo[0].path_scope", Some(6)),
                row("field_invalid", "repo[0].base_branch", Some(7)),
            ]
        );
    }

    #[test]
    fn a_hook_in_the_project_file_is_refused_so_the_repo_can_never_define_one() {
        let text = format!(
            "[project]\nname = \"shop\"\n{}\n[[hook]]\nname = \"x\"\naction = \"run_host\"\ncommand = \"id\"\n",
            repo_block("api", "default = true\n"),
        );
        let found = codes(&text);
        assert!(
            found.iter().any(|(code, field, _)| *code == "field_unknown"
                && field.as_deref().is_some_and(|f| f.starts_with("hook"))),
            "{found:?}"
        );
    }

    #[test]
    fn every_problem_is_reported_at_once_in_line_order() {
        let text = format!(
            "[project]\nname = \"\"\nowner = \"me\"\n{}{}",
            repo_block("A", "default = true\npath_scope = [\"/x\"]\n"),
            repo_block("b", "branch = \"main\"\n"),
        );
        assert_eq!(
            codes(&text),
            [
                row("field_invalid", "project.name", Some(2)),
                row("field_unknown", "project.owner", Some(3)),
                row("field_invalid", "repo[0].name", Some(5)),
                row("path_scope_invalid", "repo[0].path_scope[0]", Some(8)),
                row("field_unknown", "repo[1].branch", Some(12)),
            ]
        );
    }

    #[test]
    fn the_content_hash_is_sha256_with_a_prefix() {
        let h = sha256(b"");
        assert_eq!(h.len(), 71);
        assert_eq!(
            h,
            "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }
}
