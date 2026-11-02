use std::collections::BTreeMap;

use saka_core::vocab::Tool;
use serde_json::{Value, json};

pub const FILES: [&str; 4] = ["go.mod", "package.json", ".node-version", ".nvmrc"];

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Files {
    pub go_mod: Option<String>,
    pub package_json: Option<String>,
    pub node_version: Option<String>,
    pub nvmrc: Option<String>,
}

impl Files {
    pub fn set(&mut self, path: &str, text: Option<String>) {
        match path {
            "go.mod" => self.go_mod = text,
            "package.json" => self.package_json = text,
            ".node-version" => self.node_version = text,
            ".nvmrc" => self.nvmrc = text,
            _ => {}
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unpinned {
    pub tool: Tool,
    pub source: &'static str,
    pub value: Option<String>,
}

impl Unpinned {
    pub fn to_json(&self) -> Value {
        json!({ "tool": self.tool, "source": self.source, "value": self.value })
    }
}

fn exact(version: &str) -> bool {
    let parts: Vec<&str> = version.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.len() <= 6 && p.bytes().all(|b| b.is_ascii_digit()))
}

fn go_version(raw: &str) -> Option<String> {
    if exact(raw) {
        return Some(raw.to_string());
    }
    let (major, minor) = raw.split_once('.')?;
    let numeric = |s: &str| !s.is_empty() && s.len() <= 6 && s.bytes().all(|b| b.is_ascii_digit());
    if !numeric(major) || !numeric(minor) {
        return None;
    }
    // From Go 1.21 on, `go 1.N` names release 1.N.0; older releases are named `go1.N`.
    let modern = major != "1" || minor.parse::<u32>().is_ok_and(|m| m >= 21);
    Some(if modern {
        format!("{major}.{minor}.0")
    } else {
        format!("{major}.{minor}")
    })
}

fn pinned(tool: Tool, source: &'static str, raw: &str) -> Result<String, Unpinned> {
    let value = raw.trim();
    let version = match tool {
        Tool::Go => go_version(value.strip_prefix("go").unwrap_or(value)),
        Tool::Node => {
            Some(value.strip_prefix('v').unwrap_or(value).to_string()).filter(|v| exact(v))
        }
        Tool::Pnpm => Some(value.to_string()).filter(|v| exact(v)),
    };
    version.ok_or_else(|| Unpinned {
        tool,
        source,
        value: Some(value.to_string()),
    })
}

fn go_mod(text: &str, directive: &str) -> Option<String> {
    text.lines()
        .map(|l| l.split("//").next().unwrap_or("").trim())
        .find_map(|l| {
            l.strip_prefix(directive)
                .filter(|rest| rest.starts_with([' ', '\t']))
                .map(|rest| rest.trim().to_string())
        })
}

fn engine<'a>(package: &'a Value, field: &str, name: &str) -> Option<&'a Value> {
    let entry = package.get("devEngines")?.get(field)?;
    let named = |v: &&Value| v.get("name").and_then(Value::as_str) == Some(name);
    match entry {
        Value::Array(items) => items.iter().find(named),
        other => Some(other).filter(named),
    }
}

fn version_file(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with('#'))
        .map(String::from)
}

fn first_found(
    tool: Tool,
    candidates: Vec<(&'static str, Option<String>)>,
) -> Option<Result<String, Unpinned>> {
    candidates
        .into_iter()
        .find_map(|(source, value)| value.map(|v| pinned(tool, source, &v)))
}

pub fn detect(
    recipe: &BTreeMap<Tool, String>,
    files: &Files,
) -> Result<Vec<(Tool, String)>, Unpinned> {
    let package: Option<Value> = files
        .package_json
        .as_deref()
        .and_then(|t| serde_json::from_str(t).ok());
    let package = package.as_ref();
    let dev_engine = |field: &str, name: &str| {
        package.and_then(|p| engine(p, field, name)).map(|e| {
            e.get("version")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string()
        })
    };
    let package_manager = package
        .and_then(|p| p.get("packageManager"))
        .and_then(Value::as_str)
        .and_then(|pm| pm.strip_prefix("pnpm@"))
        .map(|v| v.split('+').next().unwrap_or("").to_string());

    let go = first_found(
        Tool::Go,
        vec![
            ("recipe", recipe.get(&Tool::Go).cloned()),
            (
                "go_mod_toolchain",
                files.go_mod.as_deref().and_then(|t| go_mod(t, "toolchain")),
            ),
            (
                "go_mod_go",
                files.go_mod.as_deref().and_then(|t| go_mod(t, "go")),
            ),
        ],
    );
    let node = first_found(
        Tool::Node,
        vec![
            ("recipe", recipe.get(&Tool::Node).cloned()),
            ("package_json_dev_engines", dev_engine("runtime", "node")),
            (
                "node_version_file",
                files.node_version.as_deref().and_then(version_file),
            ),
            ("nvmrc", files.nvmrc.as_deref().and_then(version_file)),
        ],
    );
    let pnpm = first_found(
        Tool::Pnpm,
        vec![
            ("recipe", recipe.get(&Tool::Pnpm).cloned()),
            ("package_json_package_manager", package_manager),
            (
                "package_json_dev_engines",
                dev_engine("packageManager", "pnpm"),
            ),
        ],
    );
    if pnpm.is_some() && node.is_none() {
        return Err(Unpinned {
            tool: Tool::Node,
            source: "pnpm_requires_node",
            value: None,
        });
    }
    let mut out = Vec::new();
    for (tool, found) in [(Tool::Go, go), (Tool::Node, node), (Tool::Pnpm, pnpm)] {
        if let Some(found) = found {
            out.push((tool, found?));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(go_mod: &str, package_json: &str, node_version: &str, nvmrc: &str) -> Files {
        let opt = |s: &str| (!s.is_empty()).then(|| s.to_string());
        Files {
            go_mod: opt(go_mod),
            package_json: opt(package_json),
            node_version: opt(node_version),
            nvmrc: opt(nvmrc),
        }
    }

    fn found(f: &Files) -> Vec<(Tool, String)> {
        detect(&BTreeMap::new(), f).unwrap()
    }

    fn unpinned(
        recipe: &BTreeMap<Tool, String>,
        f: &Files,
    ) -> (Tool, &'static str, Option<String>) {
        let u = detect(recipe, f).unwrap_err();
        (u.tool, u.source, u.value)
    }

    #[test]
    fn a_repo_without_declarations_installs_nothing() {
        assert!(found(&Files::default()).is_empty());
        assert!(found(&files("module x\n", "{\"name\":\"x\"}", "", "")).is_empty());
    }

    #[test]
    fn go_takes_the_recipe_then_the_toolchain_line_then_the_go_line() {
        let gomod = "module x\n\ngo 1.26 // language\ntoolchain go1.26.3\n";
        assert_eq!(
            found(&files(gomod, "", "", "")),
            [(Tool::Go, "1.26.3".into())]
        );
        assert_eq!(
            found(&files("module x\ngo 1.26.2\n", "", "", "")),
            [(Tool::Go, "1.26.2".into())]
        );
        assert_eq!(
            found(&files("module x\ngo 1.26\n", "", "", "")),
            [(Tool::Go, "1.26.0".into())]
        );
        assert_eq!(
            found(&files("module x\ngo 1.20\n", "", "", "")),
            [(Tool::Go, "1.20".into())]
        );
        let recipe = BTreeMap::from([(Tool::Go, "1.27.1".to_string())]);
        assert_eq!(
            detect(&recipe, &files(gomod, "", "", "")).unwrap(),
            [(Tool::Go, "1.27.1".into())]
        );
        assert_eq!(
            unpinned(&BTreeMap::new(), &files("go 1.27rc1\n", "", "", "")),
            (Tool::Go, "go_mod_go", Some("1.27rc1".into()))
        );
        assert_eq!(
            unpinned(
                &BTreeMap::new(),
                &files("go 1.26\ntoolchain default\n", "", "", "")
            ),
            (Tool::Go, "go_mod_toolchain", Some("default".into()))
        );
    }

    #[test]
    fn node_takes_the_recipe_then_dev_engines_then_version_files() {
        let pkg =
            r#"{"devEngines":{"runtime":{"name":"node","version":"26.13.0","onFail":"download"}}}"#;
        assert_eq!(
            found(&files("", pkg, "24.1.0", "22.0.0")),
            [(Tool::Node, "26.13.0".into())]
        );
        let array = r#"{"devEngines":{"runtime":[{"name":"bun","version":"1.2.0"},{"name":"node","version":"v26.13.0"}]}}"#;
        assert_eq!(
            found(&files("", array, "", "")),
            [(Tool::Node, "26.13.0".into())]
        );
        assert_eq!(
            found(&files("", "{}", "v24.1.0\n", "22.0.0")),
            [(Tool::Node, "24.1.0".into())]
        );
        assert_eq!(
            found(&files("", "", "", "# pinned\n22.0.0\n")),
            [(Tool::Node, "22.0.0".into())]
        );
        for (pkg, node_version, nvmrc, source, value) in [
            (
                r#"{"devEngines":{"runtime":{"name":"node","version":">=26"}}}"#,
                "",
                "",
                "package_json_dev_engines",
                ">=26",
            ),
            ("", "26", "", "node_version_file", "26"),
            ("", "", "lts/*", "nvmrc", "lts/*"),
            ("", "", "latest", "nvmrc", "latest"),
        ] {
            assert_eq!(
                unpinned(&BTreeMap::new(), &files("", pkg, node_version, nvmrc)),
                (Tool::Node, source, Some(value.to_string()))
            );
        }
    }

    #[test]
    fn pnpm_needs_an_exact_version_and_a_declared_node() {
        let pkg = r#"{"packageManager":"pnpm@12.16.1+sha512.abc","devEngines":{"runtime":{"name":"node","version":"26.13.0"}}}"#;
        assert_eq!(
            found(&files("", pkg, "", "")),
            [
                (Tool::Node, "26.13.0".into()),
                (Tool::Pnpm, "12.16.1".into())
            ]
        );
        let engines = r#"{"devEngines":{"packageManager":{"name":"pnpm","version":"12.16.1"}}}"#;
        assert_eq!(
            found(&files("", engines, "26.13.0", "")),
            [
                (Tool::Node, "26.13.0".into()),
                (Tool::Pnpm, "12.16.1".into())
            ]
        );
        assert!(found(&files("", r#"{"packageManager":"yarn@4.1.0"}"#, "", "")).is_empty());
        assert_eq!(
            unpinned(
                &BTreeMap::new(),
                &files("", r#"{"packageManager":"pnpm@12.16.1"}"#, "", "")
            ),
            (Tool::Node, "pnpm_requires_node", None)
        );
        assert_eq!(
            unpinned(
                &BTreeMap::new(),
                &files("", r#"{"packageManager":"pnpm@12"}"#, "26.13.0", "")
            ),
            (
                Tool::Pnpm,
                "package_json_package_manager",
                Some("12".into())
            )
        );
        let recipe = BTreeMap::from([(Tool::Pnpm, "^12".to_string())]);
        assert_eq!(
            unpinned(&recipe, &files("", "", "26.13.0", "")),
            (Tool::Pnpm, "recipe", Some("^12".into()))
        );
    }
}
