use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use saka_core::vocab::Tool;
use saka_ports::{Release, ReleaseIndex};
use serde::Serialize;
use serde_json::{Value, json};

use crate::recipe::{self, RECIPE_PATH};
use crate::toolchain::{self, Files, exact, version_file};

mod evidence;

pub const MAX_FILE_BYTES: usize = 256 * 1024;
pub const MAX_WORKFLOWS: usize = 64;

const WORKFLOW_DIR: &str = ".github/workflows/";
const CONTENT_FILES: &[&str] = &[
    "go.mod",
    "package.json",
    ".node-version",
    ".nvmrc",
    ".tool-versions",
    "rust-toolchain.toml",
    "rust-toolchain",
    "Cargo.toml",
    ".python-version",
    "pyproject.toml",
    "Dockerfile",
    RECIPE_PATH,
];
const PRESENCE_FILES: &[&str] = &[
    "pnpm-lock.yaml",
    "package-lock.json",
    "npm-shrinkwrap.json",
    "yarn.lock",
    "bun.lock",
    "bun.lockb",
    "go.sum",
    "Cargo.lock",
    "uv.lock",
    "poetry.lock",
    "requirements.txt",
    "setup.py",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileUse {
    Content,
    Presence,
}

fn is_workflow(path: &str) -> bool {
    path.strip_prefix(WORKFLOW_DIR).is_some_and(|name| {
        !name.contains('/')
            && name
                .strip_suffix(".yml")
                .or_else(|| name.strip_suffix(".yaml"))
                .is_some_and(|stem| !stem.is_empty())
    })
}

fn is_dockerfile(path: &str) -> bool {
    path == "Dockerfile"
        || (!path.contains('/')
            && path
                .strip_suffix(".Dockerfile")
                .is_some_and(|stem| !stem.is_empty()))
}

pub fn file_use(path: &str) -> Option<FileUse> {
    if CONTENT_FILES.contains(&path) || is_dockerfile(path) || is_workflow(path) {
        Some(FileUse::Content)
    } else if PRESENCE_FILES.contains(&path) {
        Some(FileUse::Presence)
    } else {
        None
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileSkipped {
    pub path: String,
    pub reason: &'static str,
}

#[derive(Debug, Clone, Default)]
pub struct RepoFiles {
    texts: BTreeMap<String, String>,
    present: BTreeSet<String>,
    skipped: Vec<FileSkipped>,
}

impl RepoFiles {
    pub fn new() -> RepoFiles {
        RepoFiles::default()
    }

    pub fn add(&mut self, path: &str, bytes: Vec<u8>) {
        self.present.insert(path.to_string());
        if file_use(path) != Some(FileUse::Content) {
            return;
        }
        if bytes.len() > MAX_FILE_BYTES {
            self.skip(path, "too_large");
            return;
        }
        match String::from_utf8(bytes) {
            Ok(text) => {
                self.texts.insert(path.to_string(), text);
            }
            Err(_) => self.skip(path, "not_utf8"),
        }
    }

    pub fn add_present(&mut self, path: &str) {
        self.present.insert(path.to_string());
    }

    pub fn text(&self, path: &str) -> Option<&str> {
        self.texts.get(path).map(String::as_str)
    }

    pub fn has(&self, path: &str) -> bool {
        self.present.contains(path)
    }

    pub fn skipped(&self) -> &[FileSkipped] {
        &self.skipped
    }

    fn skip(&mut self, path: &str, reason: &'static str) {
        self.skipped.push(FileSkipped {
            path: path.to_string(),
            reason,
        });
    }

    fn texts_where(&self, keep: fn(&str) -> bool) -> impl Iterator<Item = (&str, &str)> {
        self.texts
            .iter()
            .filter(move |(p, _)| keep(p))
            .map(|(p, t)| (p.as_str(), t.as_str()))
    }
}

impl<'a> FromIterator<(&'a str, &'a str)> for RepoFiles {
    fn from_iter<I: IntoIterator<Item = (&'a str, &'a str)>>(iter: I) -> RepoFiles {
        let mut files = RepoFiles::new();
        for (path, text) in iter {
            files.add(path, text.as_bytes().to_vec());
        }
        files
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VersionTool {
    Go,
    Node,
    Pnpm,
    Rust,
    Python,
}

impl VersionTool {
    pub const ALL: [VersionTool; 5] = [
        VersionTool::Go,
        VersionTool::Node,
        VersionTool::Pnpm,
        VersionTool::Rust,
        VersionTool::Python,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            VersionTool::Go => "go",
            VersionTool::Node => "node",
            VersionTool::Pnpm => "pnpm",
            VersionTool::Rust => "rust",
            VersionTool::Python => "python",
        }
    }

    pub fn provisioned(self) -> Option<Tool> {
        match self {
            VersionTool::Go => Some(Tool::Go),
            VersionTool::Node => Some(Tool::Node),
            VersionTool::Pnpm => Some(Tool::Pnpm),
            VersionTool::Rust | VersionTool::Python => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VersionStatus {
    Pinned,
    Unpinned,
    EvidenceOnly,
    Undeclared,
}

impl VersionStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            VersionStatus::Pinned => "pinned",
            VersionStatus::Unpinned => "unpinned",
            VersionStatus::EvidenceOnly => "evidence_only",
            VersionStatus::Undeclared => "undeclared",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Evidence {
    pub source: &'static str,
    pub file: String,
    pub line: usize,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Drift {
    pub source: &'static str,
    pub file: String,
    pub line: usize,
    pub value: String,
    pub pinned: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProposalAction {
    None,
    WriteFile,
    ReplaceFile,
    Manual,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Proposal {
    pub action: ProposalAction,
    pub path: Option<String>,
    pub version: Option<String>,
    pub content: Option<String>,
    pub command: Option<String>,
    pub code: Option<&'static str>,
    pub params: Value,
    pub written: bool,
}

impl Proposal {
    fn none() -> Proposal {
        Proposal {
            action: ProposalAction::None,
            path: None,
            version: None,
            content: None,
            command: None,
            code: None,
            params: json!({}),
            written: false,
        }
    }

    fn file(action: ProposalAction, path: &str, version: &str, content: String) -> Proposal {
        Proposal {
            action,
            path: Some(path.to_string()),
            version: Some(version.to_string()),
            content: Some(content),
            ..Proposal::none()
        }
    }

    fn manual(code: &'static str, version: Option<&str>, command: Option<String>) -> Proposal {
        Proposal {
            action: ProposalAction::Manual,
            version: version.map(String::from),
            command,
            code: Some(code),
            ..Proposal::none()
        }
    }

    pub fn writes_file(&self) -> bool {
        matches!(
            self.action,
            ProposalAction::WriteFile | ProposalAction::ReplaceFile
        )
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ToolReport {
    pub tool: VersionTool,
    pub status: VersionStatus,
    pub version: Option<String>,
    pub source: Option<&'static str>,
    pub value: Option<String>,
    pub provisioned_by_saka: bool,
    pub evidence: Vec<Evidence>,
    pub drift: Vec<Drift>,
    pub proposal: Proposal,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VersionReport {
    pub toolchains: Vec<ToolReport>,
    pub files_skipped: Vec<FileSkipped>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Declaration {
    source: &'static str,
    value: String,
    version: Option<String>,
}

struct Found {
    tool: VersionTool,
    declaration: Option<Declaration>,
    evidence: Vec<Evidence>,
}

fn env_files(files: &RepoFiles) -> Files {
    let mut env = Files::default();
    for path in toolchain::FILES {
        env.set(path, files.text(path).map(String::from));
    }
    env
}

fn recipe_toolchain(files: &RepoFiles, tool: Tool) -> Option<String> {
    let text = files.text(RECIPE_PATH)?;
    recipe::parse(text, u16::MAX)
        .ok()?
        .toolchain
        .get(&tool)
        .cloned()
}

fn rust_channel(text: &str) -> Option<String> {
    let doc: toml::Table = toml::from_str(text).ok()?;
    doc.get("toolchain")?
        .get("channel")?
        .as_str()
        .map(|c| c.trim().to_string())
}

fn rust_declaration(files: &RepoFiles) -> Option<(&'static str, String)> {
    if let Some(channel) = files.text("rust-toolchain.toml").and_then(rust_channel) {
        return Some(("rust_toolchain_toml", channel));
    }
    let legacy = files.text("rust-toolchain")?;
    rust_channel(legacy)
        .or_else(|| {
            legacy
                .lines()
                .map(str::trim)
                .find(|l| !l.is_empty())
                .map(String::from)
        })
        .map(|value| ("rust_toolchain_file", value))
}

fn exact_numeric(raw: &str) -> Option<String> {
    let value = raw.trim();
    exact(value).then(|| value.to_string())
}

fn declaration(tool: VersionTool, files: &RepoFiles) -> Option<Declaration> {
    if let Some(env_tool) = tool.provisioned() {
        if let Some(value) = recipe_toolchain(files, env_tool) {
            return Some(Declaration {
                source: "recipe",
                version: toolchain::exact_version(env_tool, &value),
                value: value.trim().to_string(),
            });
        }
        return toolchain::declared(env_tool, &env_files(files)).map(|d| Declaration {
            source: d.source,
            value: d.value,
            version: d.version,
        });
    }
    let (source, value) = match tool {
        VersionTool::Rust => rust_declaration(files)?,
        _ => (
            "python_version_file",
            files.text(".python-version").and_then(version_file)?,
        ),
    };
    Some(Declaration {
        source,
        version: exact_numeric(&value),
        value,
    })
}

fn relevant(tool: VersionTool, files: &RepoFiles) -> bool {
    match tool {
        VersionTool::Go => files.has("go.mod"),
        VersionTool::Node => files.has("package.json"),
        VersionTool::Pnpm => files.has("pnpm-lock.yaml"),
        VersionTool::Rust => files.has("Cargo.toml"),
        VersionTool::Python => [
            "pyproject.toml",
            "requirements.txt",
            "setup.py",
            ".python-version",
        ]
        .iter()
        .any(|p| files.has(p)),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Hint {
    Exact(Vec<u64>),
    Prefix {
        prefix: Vec<u64>,
        min: Option<Vec<u64>>,
    },
    LatestLts,
    NamedLts(String),
    Unsupported,
}

fn number(part: &str) -> Option<u64> {
    (!part.is_empty() && part.len() <= 6 && part.bytes().all(|b| b.is_ascii_digit()))
        .then(|| part.parse().ok())
        .flatten()
}

fn numbers(version: &str) -> Option<Vec<u64>> {
    version.split('.').map(number).collect()
}

fn hint(tool: VersionTool, raw: &str) -> Hint {
    let value = raw.trim();
    if tool == VersionTool::Node {
        let lower = value.to_ascii_lowercase();
        if lower == "lts/*" {
            return Hint::LatestLts;
        }
        if let Some(name) = lower
            .strip_prefix("lts/")
            .filter(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))
        {
            return Hint::NamedLts(name.to_string());
        }
    }
    let ranged = |prefix_len: usize, rest: &str| match numbers(rest) {
        Some(min) if min.len() == 3 => match min.get(..prefix_len) {
            Some(prefix) => Hint::Prefix {
                prefix: prefix.to_vec(),
                min: Some(min),
            },
            None => Hint::Unsupported,
        },
        _ => Hint::Unsupported,
    };
    let strip = |v: &'_ str| -> String {
        let v = v.strip_prefix('v').unwrap_or(v);
        let v = if tool == VersionTool::Go {
            v.strip_prefix("go").unwrap_or(v)
        } else {
            v
        };
        v.to_string()
    };
    if let Some(rest) = value.strip_prefix('^') {
        return ranged(1, &strip(rest));
    }
    if let Some(rest) = value.strip_prefix('~') {
        return ranged(2, &strip(rest));
    }
    let value = strip(value);
    let mut parts: Vec<&str> = value.split('.').collect();
    let wildcard = parts.len() > 1 && matches!(parts.last(), Some(&"x" | &"*"));
    if wildcard {
        parts.pop();
    }
    let Some(prefix) = parts
        .iter()
        .map(|p| number(p))
        .collect::<Option<Vec<u64>>>()
    else {
        return Hint::Unsupported;
    };
    match (prefix.len(), wildcard) {
        (3, false) => Hint::Exact(prefix),
        (1 | 2, _) => Hint::Prefix { prefix, min: None },
        _ => Hint::Unsupported,
    }
}

fn joined(parts: &[u64]) -> String {
    parts
        .iter()
        .map(u64::to_string)
        .collect::<Vec<_>>()
        .join(".")
}

fn matches_range(version: &[u64], prefix: &[u64], min: Option<&Vec<u64>>) -> bool {
    version.starts_with(prefix) && min.is_none_or(|m| version.cmp(m.as_slice()) != Ordering::Less)
}

fn pick(releases: &[Release], hint: &Hint) -> Option<String> {
    releases
        .iter()
        .filter_map(|r| numbers(&r.version).map(|n| (n, r)))
        .filter(|(n, r)| match hint {
            Hint::Prefix { prefix, min } => matches_range(n, prefix, min.as_ref()),
            Hint::LatestLts => r.lts.is_some(),
            Hint::NamedLts(name) => r
                .lts
                .as_deref()
                .is_some_and(|l| l.eq_ignore_ascii_case(name)),
            Hint::Exact(_) | Hint::Unsupported => false,
        })
        .max_by(|(a, _), (b, _)| a.cmp(b))
        .map(|(n, _)| joined(&n))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Unresolved {
    NoHints,
    UnsupportedHint,
    NoReleaseIndex,
    NoMatchingRelease,
    IndexUnavailable,
}

impl Unresolved {
    fn as_str(self) -> &'static str {
        match self {
            Unresolved::NoHints => "no_hints",
            Unresolved::UnsupportedHint => "unsupported_hint",
            Unresolved::NoReleaseIndex => "no_release_index",
            Unresolved::NoMatchingRelease => "no_matching_release",
            Unresolved::IndexUnavailable => "index_unavailable",
        }
    }
}

async fn resolve(
    tool: VersionTool,
    hints: &[String],
    index: &dyn ReleaseIndex,
) -> Result<String, Unresolved> {
    let mut releases: Option<Option<Vec<Release>>> = None;
    let mut worst = Unresolved::NoHints;
    for raw in hints {
        let parsed = hint(tool, raw);
        let reason = match (&parsed, tool.provisioned()) {
            (Hint::Exact(parts), _) => return Ok(joined(parts)),
            (Hint::Unsupported, _) => Unresolved::UnsupportedHint,
            (_, None) => Unresolved::NoReleaseIndex,
            (_, Some(env_tool)) => {
                if releases.is_none() {
                    releases = Some(index.releases(env_tool).await.ok());
                }
                match releases.as_ref().and_then(Option::as_ref) {
                    None => Unresolved::IndexUnavailable,
                    Some(list) => match pick(list, &parsed) {
                        Some(version) => return Ok(version),
                        None => Unresolved::NoMatchingRelease,
                    },
                }
            }
        };
        worst = worst.max(reason);
    }
    Err(worst)
}

fn drift(pinned: &str, tool: VersionTool, evidence: &[Evidence]) -> Vec<Drift> {
    let Some(have) = numbers(pinned) else {
        return Vec::new();
    };
    evidence
        .iter()
        .filter(|e| match hint(tool, &e.value) {
            Hint::Exact(want) => want != have,
            Hint::Prefix { prefix, min } => !matches_range(&have, &prefix, min.as_ref()),
            Hint::LatestLts | Hint::NamedLts(_) | Hint::Unsupported => false,
        })
        .map(|e| Drift {
            source: e.source,
            file: e.file.clone(),
            line: e.line,
            value: e.value.clone(),
            pinned: pinned.to_string(),
        })
        .collect()
}

fn version_line(path: &str, version: &str, files: &RepoFiles) -> Proposal {
    let action = if files.has(path) {
        ProposalAction::ReplaceFile
    } else {
        ProposalAction::WriteFile
    };
    Proposal::file(action, path, version, format!("{version}\n"))
}

fn rust_toolchain_toml(version: &str) -> String {
    let channel = toml::Value::String(version.to_string());
    format!("[toolchain]\nchannel = {channel}\n")
}

fn proposal(found: &Found, version: &str, files: &RepoFiles) -> Proposal {
    let source = found.declaration.as_ref().map(|d| d.source);
    match found.tool {
        VersionTool::Node => match source {
            Some("package_json_dev_engines") => Proposal::manual(
                "set_dev_engines_runtime",
                Some(version),
                Some(format!(
                    "npm pkg set devEngines.runtime.name=node devEngines.runtime.version={version}"
                )),
            ),
            Some("node_version_file") => version_line(".node-version", version, files),
            Some("recipe") => recipe_manual(found.tool, version),
            _ => version_line(".nvmrc", version, files),
        },
        VersionTool::Pnpm => match source {
            Some("package_json_dev_engines") => Proposal::manual(
                "set_dev_engines_package_manager",
                Some(version),
                Some(format!(
                    "npm pkg set devEngines.packageManager.version={version}"
                )),
            ),
            Some("recipe") => recipe_manual(found.tool, version),
            _ => Proposal::manual(
                "set_package_manager",
                Some(version),
                Some(format!("npm pkg set packageManager=pnpm@{version}")),
            ),
        },
        VersionTool::Go => match source {
            Some("go_mod_toolchain") => Proposal::manual(
                "set_go_toolchain",
                Some(version),
                Some(format!("go mod edit -toolchain=go{version}")),
            ),
            Some("recipe") => recipe_manual(found.tool, version),
            _ => Proposal::manual(
                "set_go_directive",
                Some(version),
                Some(format!("go mod edit -go={version}")),
            ),
        },
        VersionTool::Rust => {
            if source == Some("rust_toolchain_file") {
                Proposal::file(
                    ProposalAction::ReplaceFile,
                    "rust-toolchain",
                    version,
                    format!("{version}\n"),
                )
            } else if files.has("rust-toolchain.toml") {
                Proposal {
                    params: json!({ "path": "rust-toolchain.toml", "channel": version }),
                    ..Proposal::manual("set_rust_channel", Some(version), None)
                }
            } else if files.has("rust-toolchain") {
                version_line("rust-toolchain", version, files)
            } else {
                Proposal::file(
                    ProposalAction::WriteFile,
                    "rust-toolchain.toml",
                    version,
                    rust_toolchain_toml(version),
                )
            }
        }
        VersionTool::Python => version_line(".python-version", version, files),
    }
}

fn recipe_manual(tool: VersionTool, version: &str) -> Proposal {
    Proposal {
        params: json!({ "path": RECIPE_PATH, "field": format!("toolchain.{}", tool.as_str()) }),
        ..Proposal::manual("set_recipe_toolchain", Some(version), None)
    }
}

fn unresolved(tool: VersionTool, hints: &[String], reason: Unresolved) -> Proposal {
    Proposal {
        params: json!({ "tool": tool.as_str(), "hints": hints, "reason": reason.as_str() }),
        ..Proposal::manual("version_unresolved", None, None)
    }
}

fn scan(files: &RepoFiles) -> (Vec<Found>, Vec<FileSkipped>) {
    let (all, skipped) = evidence::collect(files);
    let found = VersionTool::ALL
        .into_iter()
        .filter_map(|tool| {
            let declaration = declaration(tool, files);
            (declaration.is_some() || relevant(tool, files)).then(|| Found {
                tool,
                declaration,
                evidence: all
                    .iter()
                    .filter(|(t, _)| *t == tool)
                    .map(|(_, e)| e.clone())
                    .collect(),
            })
        })
        .collect();
    (found, skipped)
}

pub async fn detect(files: &RepoFiles, index: &dyn ReleaseIndex) -> VersionReport {
    let (found, skipped) = scan(files);
    let mut toolchains = Vec::with_capacity(found.len());
    for found in found {
        let declaration = found.declaration.clone();
        let status = match (&declaration, found.evidence.is_empty()) {
            (Some(d), _) if d.version.is_some() => VersionStatus::Pinned,
            (Some(_), _) => VersionStatus::Unpinned,
            (None, false) => VersionStatus::EvidenceOnly,
            (None, true) => VersionStatus::Undeclared,
        };
        let pinned = declaration.as_ref().and_then(|d| d.version.clone());
        let (drift, proposal) = match &pinned {
            Some(version) => (
                drift(version, found.tool, &found.evidence),
                Proposal::none(),
            ),
            None => {
                let hints: Vec<String> = declaration
                    .iter()
                    .map(|d| d.value.clone())
                    .chain(found.evidence.iter().map(|e| e.value.clone()))
                    .collect();
                let proposal = match resolve(found.tool, &hints, index).await {
                    Ok(version) => proposal(&found, &version, files),
                    Err(reason) => unresolved(found.tool, &hints, reason),
                };
                (Vec::new(), proposal)
            }
        };
        toolchains.push(ToolReport {
            tool: found.tool,
            status,
            version: pinned,
            source: declaration.as_ref().map(|d| d.source),
            value: declaration.map(|d| d.value),
            provisioned_by_saka: found.tool.provisioned().is_some(),
            evidence: found.evidence,
            drift,
            proposal,
        });
    }
    let mut files_skipped = files.skipped().to_vec();
    files_skipped.extend(skipped);
    VersionReport {
        toolchains,
        files_skipped,
    }
}
