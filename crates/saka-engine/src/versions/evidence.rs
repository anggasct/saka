use saphyr::{MarkedYaml, YamlData, YamlLoader};
use saphyr_parser::Parser;
use serde::Deserialize;
use serde_json::Value;
use toml::Spanned;

use super::{
    Evidence, FileSkipped, MAX_WORKFLOWS, RepoFiles, VersionTool, is_dockerfile, is_workflow,
};
use crate::recipe::line_of;

const YAML_ALIAS_LIMIT: usize = 10_000;

type Found = (VersionTool, Evidence);

fn rank(source: &str) -> u8 {
    match source {
        "cargo_rust_version" | "pyproject_requires_python" => 0,
        "tool_versions" => 1,
        "package_json_engines" => 2,
        "github_actions" => 3,
        _ => 4,
    }
}

fn evidence(
    tool: VersionTool,
    source: &'static str,
    file: &str,
    line: usize,
    value: &str,
) -> Found {
    (
        tool,
        Evidence {
            source,
            file: file.to_string(),
            line,
            value: value.trim().to_string(),
        },
    )
}

fn tool_versions(text: &str, out: &mut Vec<Found>) {
    for (n, line) in text.lines().enumerate() {
        let line = line.split('#').next().unwrap_or("");
        let mut words = line.split_whitespace();
        let (Some(plugin), Some(version)) = (words.next(), words.next()) else {
            continue;
        };
        let tool = match plugin {
            "golang" => VersionTool::Go,
            "nodejs" => VersionTool::Node,
            "pnpm" => VersionTool::Pnpm,
            "rust" => VersionTool::Rust,
            "python" => VersionTool::Python,
            _ => continue,
        };
        out.push(evidence(
            tool,
            "tool_versions",
            ".tool-versions",
            n + 1,
            version,
        ));
    }
}

fn json_key_line(text: &str, keys: &[&str]) -> usize {
    let mut at = 0;
    for key in keys {
        let quoted = format!("\"{key}\"");
        match text.get(at..).and_then(|rest| rest.find(&quoted)) {
            Some(found) => at += found + quoted.len(),
            None => break,
        }
    }
    line_of(text, at)
}

fn package_engines(text: &str, out: &mut Vec<Found>) {
    let Ok(package) = serde_json::from_str::<Value>(text) else {
        return;
    };
    let Some(engines) = package.get("engines") else {
        return;
    };
    for (key, tool) in [("node", VersionTool::Node), ("pnpm", VersionTool::Pnpm)] {
        if let Some(value) = engines.get(key).and_then(Value::as_str) {
            let line = json_key_line(text, &["engines", key]);
            out.push(evidence(
                tool,
                "package_json_engines",
                "package.json",
                line,
                value,
            ));
        }
    }
}

#[derive(Deserialize)]
struct CargoPackage {
    #[serde(rename = "rust-version")]
    rust_version: Option<Spanned<toml::Value>>,
}

#[derive(Deserialize)]
struct CargoWorkspace {
    package: Option<CargoPackage>,
}

#[derive(Deserialize)]
struct CargoManifest {
    package: Option<CargoPackage>,
    workspace: Option<CargoWorkspace>,
}

fn spanned_string(text: &str, value: Option<&Spanned<toml::Value>>) -> Option<(usize, String)> {
    let value = value?;
    let string = value.get_ref().as_str()?;
    Some((line_of(text, value.span().start), string.to_string()))
}

fn cargo_rust_version(text: &str, out: &mut Vec<Found>) {
    let Ok(manifest) = toml::from_str::<CargoManifest>(text) else {
        return;
    };
    let package = manifest
        .package
        .as_ref()
        .and_then(|p| p.rust_version.as_ref());
    let workspace = manifest
        .workspace
        .as_ref()
        .and_then(|w| w.package.as_ref())
        .and_then(|p| p.rust_version.as_ref());
    let found = spanned_string(text, package).or_else(|| spanned_string(text, workspace));
    if let Some((line, value)) = found {
        out.push(evidence(
            VersionTool::Rust,
            "cargo_rust_version",
            "Cargo.toml",
            line,
            &value,
        ));
    }
}

#[derive(Deserialize)]
struct PyProject {
    project: Option<PyProjectTable>,
}

#[derive(Deserialize)]
struct PyProjectTable {
    #[serde(rename = "requires-python")]
    requires_python: Option<Spanned<toml::Value>>,
}

fn pyproject(text: &str, out: &mut Vec<Found>) {
    let Ok(doc) = toml::from_str::<PyProject>(text) else {
        return;
    };
    let value = doc
        .project
        .as_ref()
        .and_then(|p| p.requires_python.as_ref());
    if let Some((line, value)) = spanned_string(text, value) {
        out.push(evidence(
            VersionTool::Python,
            "pyproject_requires_python",
            "pyproject.toml",
            line,
            &value,
        ));
    }
}

fn scalar<'a>(node: &'a MarkedYaml<'_>) -> Option<&'a str> {
    match &node.data {
        YamlData::Representation(text, _, _) => Some(text),
        _ => None,
    }
}

fn entries<'a, 'i>(
    node: &'a MarkedYaml<'i>,
) -> impl Iterator<Item = (&'a str, &'a MarkedYaml<'i>, usize)> {
    let mapping = match &node.data {
        YamlData::Mapping(mapping) => Some(mapping),
        _ => None,
    };
    mapping
        .into_iter()
        .flat_map(|m| m.iter())
        .filter_map(|(k, v)| scalar(k).map(|key| (key, v, k.span.start.line())))
}

fn field<'a, 'i>(node: &'a MarkedYaml<'i>, name: &str) -> Option<(&'a MarkedYaml<'i>, usize)> {
    entries(node)
        .find(|(k, _, _)| *k == name)
        .map(|(_, v, line)| (v, line))
}

fn sequence<'a, 'i>(node: &'a MarkedYaml<'i>) -> &'a [MarkedYaml<'i>] {
    match &node.data {
        YamlData::Sequence(items) => items,
        _ => &[],
    }
}

fn setup_input(action: &str) -> Option<(VersionTool, &'static str)> {
    match action {
        "actions/setup-go" => Some((VersionTool::Go, "go-version")),
        "actions/setup-node" => Some((VersionTool::Node, "node-version")),
        "pnpm/action-setup" => Some((VersionTool::Pnpm, "version")),
        "actions/setup-python" => Some((VersionTool::Python, "python-version")),
        "dtolnay/rust-toolchain" => Some((VersionTool::Rust, "toolchain")),
        _ => None,
    }
}

fn step(path: &str, step: &MarkedYaml<'_>, out: &mut Vec<Found>) {
    let Some((uses, uses_line)) = field(step, "uses") else {
        return;
    };
    let Some((action, reference)) = scalar(uses).and_then(|u| u.split_once('@')) else {
        return;
    };
    let action = action.to_ascii_lowercase();
    let Some((tool, input)) = setup_input(&action) else {
        return;
    };
    let mut push = |line: usize, value: &str| {
        if !value.contains("${{") && !value.trim().is_empty() {
            out.push(evidence(tool, "github_actions", path, line, value));
        }
    };
    if tool == VersionTool::Rust && reference.starts_with(|c: char| c.is_ascii_digit()) {
        push(uses_line, reference);
        return;
    }
    let Some((with, _)) = field(step, "with") else {
        return;
    };
    if let Some((value, line)) = field(with, input)
        && let Some(value) = scalar(value)
    {
        push(line, value);
    }
}

fn workflow(path: &str, text: &str, out: &mut Vec<Found>) -> Result<(), ()> {
    let mut loader: YamlLoader<'_, MarkedYaml<'_>> = YamlLoader::default();
    loader.early_parse(false);
    loader.alias_limit(YAML_ALIAS_LIMIT);
    Parser::new_from_str(text)
        .load(&mut loader, true)
        .map_err(|_| ())?;
    if loader.error().is_some() {
        return Err(());
    }
    for doc in loader.into_documents() {
        let Some((jobs, _)) = field(&doc, "jobs") else {
            continue;
        };
        for (_, job, _) in entries(jobs) {
            if let Some((steps, _)) = field(job, "steps") {
                for s in sequence(steps) {
                    step(path, s, out);
                }
            }
        }
    }
    Ok(())
}

fn image_tool(name: &str) -> Option<VersionTool> {
    match name {
        "golang" => Some(VersionTool::Go),
        "node" => Some(VersionTool::Node),
        "rust" => Some(VersionTool::Rust),
        "python" => Some(VersionTool::Python),
        _ => None,
    }
}

fn dockerfile(path: &str, text: &str, out: &mut Vec<Found>) {
    for (n, line) in text.lines().enumerate() {
        let mut words = line.split_whitespace();
        if !words.next().is_some_and(|w| w.eq_ignore_ascii_case("FROM")) {
            continue;
        }
        let Some(image) = words.find(|w| !w.starts_with("--")) else {
            continue;
        };
        if image.contains('$') {
            continue;
        }
        let image = image.split('@').next().unwrap_or("");
        let last = image.rsplit('/').next().unwrap_or("");
        let Some((name, tag)) = last.split_once(':') else {
            continue;
        };
        let Some(tool) = image_tool(name) else {
            continue;
        };
        let version = tag.split('-').next().unwrap_or("");
        if !version.is_empty() {
            out.push(evidence(tool, "dockerfile", path, n + 1, version));
        }
    }
}

pub(super) fn collect(files: &RepoFiles) -> (Vec<Found>, Vec<FileSkipped>) {
    let mut out = Vec::new();
    let mut skipped = Vec::new();
    if let Some(text) = files.text(".tool-versions") {
        tool_versions(text, &mut out);
    }
    if let Some(text) = files.text("package.json") {
        package_engines(text, &mut out);
    }
    if let Some(text) = files.text("Cargo.toml") {
        cargo_rust_version(text, &mut out);
    }
    if let Some(text) = files.text("pyproject.toml") {
        pyproject(text, &mut out);
    }
    for (n, (path, text)) in files.texts_where(is_workflow).enumerate() {
        if n >= MAX_WORKFLOWS {
            skipped.push(FileSkipped {
                path: path.to_string(),
                reason: "workflow_limit",
            });
        } else if workflow(path, text, &mut out).is_err() {
            skipped.push(FileSkipped {
                path: path.to_string(),
                reason: "yaml_invalid",
            });
        }
    }
    for (path, text) in files.texts_where(is_dockerfile) {
        dockerfile(path, text, &mut out);
    }
    out.sort_by_key(|(_, e)| rank(e.source));
    (out, skipped)
}
