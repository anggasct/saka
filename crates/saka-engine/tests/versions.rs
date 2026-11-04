#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests fail loudly on a broken setup"
)]

use std::collections::BTreeMap;
use std::path::Path;

use proptest::prelude::*;
use saka_core::vocab::Tool;
use saka_engine::toolchain::{self, Files};
use saka_engine::versions::{
    self, FileUse, ProposalAction, RepoFiles, ToolReport, VersionReport, VersionStatus, VersionTool,
};
use saka_testkit::FakeReleases;
use serde_json::json;

fn block_on<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(f)
}

fn node_index() -> FakeReleases {
    FakeReleases::new()
        .with_lts(Tool::Node, "20.19.0", "Iron")
        .with(Tool::Node, &["22.1.0", "22.13.1", "23.0.0", "26.0.0"])
        .with_lts(Tool::Node, "22.14.0", "Jod")
        .with_lts(Tool::Node, "22.14.2", "Jod")
        .with_lts(Tool::Node, "22.15.1", "Jod")
        .with_lts(Tool::Node, "24.11.0", "Krypton")
        .with(Tool::Pnpm, &["9.15.9", "9.15.0", "10.0.0-rc.1"])
        .with(
            Tool::Go,
            &["1.24.3", "1.24.10", "1.25.4", "1.20", "1.20.14"],
        )
}

fn detect_with(files: &[(&str, &str)], index: &FakeReleases) -> VersionReport {
    let files: RepoFiles = files.iter().copied().collect();
    block_on(versions::detect(&files, index))
}

fn detect(files: &[(&str, &str)]) -> VersionReport {
    detect_with(files, &node_index())
}

fn tool(report: &VersionReport, tool: VersionTool) -> &ToolReport {
    report
        .toolchains
        .iter()
        .find(|t| t.tool == tool)
        .unwrap_or_else(|| panic!("{tool:?} not reported in {report:#?}"))
}

type Repo = Vec<(&'static str, &'static str)>;
type Summary<'a> = (VersionStatus, Option<&'a str>, Option<&'a str>);

fn summary(report: &ToolReport) -> Summary<'_> {
    (report.status, report.version.as_deref(), report.source)
}

fn load_fixture(name: &str) -> RepoFiles {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/versions")
        .join(name);
    let mut files = RepoFiles::new();
    let mut pending = vec![root.clone()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
                continue;
            }
            let rel = path
                .strip_prefix(&root)
                .unwrap()
                .to_str()
                .unwrap()
                .to_string();
            match versions::file_use(&rel) {
                Some(FileUse::Content) => files.add(&rel, std::fs::read(&path).unwrap()),
                Some(FileUse::Presence) => files.add_present(&rel),
                None => {}
            }
        }
    }
    files
}

#[test]
fn every_declaration_source_gives_its_status_version_and_source() {
    use VersionStatus::{Pinned, Unpinned};
    let cases: Vec<(Repo, VersionTool, Summary)> = vec![
        (
            vec![("go.mod", "module x\ngo 1.26\ntoolchain go1.26.3\n")],
            VersionTool::Go,
            (Pinned, Some("1.26.3"), Some("go_mod_toolchain")),
        ),
        (
            vec![("go.mod", "module x\ngo 1.26\n")],
            VersionTool::Go,
            (Pinned, Some("1.26.0"), Some("go_mod_go")),
        ),
        (
            vec![("go.mod", "module x\ngo 1.27rc1\n")],
            VersionTool::Go,
            (Unpinned, None, Some("go_mod_go")),
        ),
        (
            vec![(
                "package.json",
                r#"{"devEngines":{"runtime":{"name":"node","version":"26.13.0"}}}"#,
            )],
            VersionTool::Node,
            (Pinned, Some("26.13.0"), Some("package_json_dev_engines")),
        ),
        (
            vec![
                ("package.json", "{}"),
                (".node-version", "v24.1.0\n"),
                (".nvmrc", "22\n"),
            ],
            VersionTool::Node,
            (Pinned, Some("24.1.0"), Some("node_version_file")),
        ),
        (
            vec![("package.json", "{}"), (".nvmrc", "22\n")],
            VersionTool::Node,
            (Unpinned, None, Some("nvmrc")),
        ),
        (
            vec![
                (
                    "package.json",
                    r#"{"packageManager":"pnpm@9.15.9+sha512.abc"}"#,
                ),
                (".nvmrc", "22.14.0\n"),
            ],
            VersionTool::Pnpm,
            (Pinned, Some("9.15.9"), Some("package_json_package_manager")),
        ),
        (
            vec![(
                "package.json",
                r#"{"devEngines":{"packageManager":{"name":"pnpm","version":"^9"}}}"#,
            )],
            VersionTool::Pnpm,
            (Unpinned, None, Some("package_json_dev_engines")),
        ),
        (
            vec![
                ("Cargo.toml", "[package]\nname = \"x\"\n"),
                ("rust-toolchain.toml", "[toolchain]\nchannel = \"1.99.1\"\n"),
                ("rust-toolchain", "1.80.0\n"),
            ],
            VersionTool::Rust,
            (Pinned, Some("1.99.1"), Some("rust_toolchain_toml")),
        ),
        (
            vec![
                ("Cargo.toml", "[package]\nname = \"x\"\n"),
                ("rust-toolchain", "stable\n"),
            ],
            VersionTool::Rust,
            (Unpinned, None, Some("rust_toolchain_file")),
        ),
        (
            vec![
                ("pyproject.toml", "[project]\nname = \"x\"\n"),
                (".python-version", "# local\n3.12.7\n"),
            ],
            VersionTool::Python,
            (Pinned, Some("3.12.7"), Some("python_version_file")),
        ),
        (
            vec![
                ("requirements.txt", "flask\n"),
                (".python-version", "3.12\n"),
            ],
            VersionTool::Python,
            (Unpinned, None, Some("python_version_file")),
        ),
        (
            vec![
                ("go.mod", "module x\ngo 1.26.1\n"),
                (".saka/environment.toml", "[toolchain]\ngo = \"1.27.0\"\n"),
            ],
            VersionTool::Go,
            (Pinned, Some("1.27.0"), Some("recipe")),
        ),
    ];
    for (files, which, expected) in cases {
        let report = detect(&files);
        assert_eq!(summary(tool(&report, which)), expected, "{files:?}");
    }
}

#[test]
fn tools_report_evidence_only_or_undeclared_without_a_declaration() {
    let report = detect(&[
        ("go.mod", "module x\n"),
        ("Dockerfile", "FROM golang:1.24.3-bookworm\n"),
        ("Cargo.toml", "[package]\nname = \"x\"\n"),
    ]);
    assert_eq!(
        tool(&report, VersionTool::Go).status,
        VersionStatus::EvidenceOnly
    );
    assert_eq!(
        tool(&report, VersionTool::Rust).status,
        VersionStatus::Undeclared
    );
}

#[test]
fn a_repo_without_tool_markers_reports_nothing() {
    let report = detect(&[
        ("Dockerfile", "FROM node:22-alpine\n"),
        (".tool-versions", "nodejs 22.14.0\n"),
        ("README.md", "hi"),
    ]);
    assert!(report.toolchains.is_empty(), "{report:#?}");
    let report = detect(&[]);
    assert!(report.toolchains.is_empty());
}

fn env_files(
    go_mod: Option<&str>,
    package: Option<&str>,
    node: Option<&str>,
    nvmrc: Option<&str>,
) -> Files {
    let mut files = Files::default();
    files.set("go.mod", go_mod.map(String::from));
    files.set("package.json", package.map(String::from));
    files.set(".node-version", node.map(String::from));
    files.set(".nvmrc", nvmrc.map(String::from));
    files
}

proptest! {
    #[test]
    fn go_node_and_pnpm_agree_with_the_environment_detector(
        go_mod in prop::option::of(prop::sample::select(vec![
            "module x\n", "module x\ngo 1.26\n", "module x\ngo 1.20\n", "module x\ngo 1.27rc1\n",
            "module x\ngo 1.26\ntoolchain go1.26.3\n", "module x\ngo 1.26\ntoolchain default\n",
        ])),
        package in prop::option::of(prop::sample::select(vec![
            "{}", "not json", r#"{"packageManager":"pnpm@9.15.9"}"#, r#"{"packageManager":"pnpm@9"}"#,
            r#"{"packageManager":"yarn@4.1.0"}"#,
            r#"{"devEngines":{"runtime":{"name":"node","version":"26.13.0"}}}"#,
            r#"{"devEngines":{"runtime":{"name":"node","version":">=26"}}}"#,
            r#"{"devEngines":{"packageManager":{"name":"pnpm","version":"10.1.0"}},"packageManager":"pnpm@10"}"#,
            r#"{"devEngines":{"runtime":[{"name":"bun","version":"1.2.0"},{"name":"node","version":"v24.1.0"}],"packageManager":{"name":"pnpm","version":"latest"}}}"#,
        ])),
        node in prop::option::of(prop::sample::select(vec!["24.1.0\n", "v24\n", "\n", "lts/*\n"])),
        nvmrc in prop::option::of(prop::sample::select(vec!["22.14.0\n", "22\n", "# none\n"])),
    ) {
        let mut repo = RepoFiles::new();
        for (path, text) in [("go.mod", go_mod), ("package.json", package), (".node-version", node), (".nvmrc", nvmrc)] {
            if let Some(text) = text {
                repo.add(path, text.as_bytes().to_vec());
            }
        }
        let report = block_on(versions::detect(&repo, &FakeReleases::new()));
        let files = env_files(go_mod, package, node, nvmrc);
        for env_tool in [Tool::Go, Tool::Node, Tool::Pnpm] {
            let which = match env_tool {
                Tool::Go => VersionTool::Go,
                Tool::Node => VersionTool::Node,
                Tool::Pnpm => VersionTool::Pnpm,
            };
            let reported = report.toolchains.iter().find(|t| t.tool == which);
            let declared = toolchain::declared(env_tool, &files);
            match (reported, declared) {
                (Some(r), Some(d)) => {
                    prop_assert_eq!(r.source, Some(d.source));
                    prop_assert_eq!(&r.version, &d.version);
                    prop_assert_eq!(r.status == VersionStatus::Pinned, d.version.is_some());
                }
                (Some(r), None) => prop_assert!(r.source.is_none()),
                (None, Some(_)) => prop_assert!(false, "declared {:?} not reported", env_tool),
                (None, None) => {}
            }
        }
        match toolchain::detect(&BTreeMap::new(), &files) {
            Ok(pinned) => {
                for (env_tool, version) in pinned {
                    let r = report.toolchains.iter().find(|t| t.tool.provisioned() == Some(env_tool)).unwrap();
                    prop_assert_eq!(r.status, VersionStatus::Pinned);
                    prop_assert_eq!(r.version.as_deref(), Some(version.as_str()));
                }
            }
            Err(unpinned) if unpinned.source == "pnpm_requires_node" => {
                let r = report.toolchains.iter().find(|t| t.tool == VersionTool::Node);
                prop_assert!(r.is_none_or(|r| r.source.is_none()));
            }
            Err(unpinned) => {
                let r = report.toolchains.iter().find(|t| t.tool.provisioned() == Some(unpinned.tool)).unwrap();
                prop_assert_eq!(r.status, VersionStatus::Unpinned);
                prop_assert_eq!(r.source, Some(unpinned.source));
                prop_assert_eq!(&r.value, &unpinned.value);
            }
        }
    }
}

fn evidence_of(
    report: &VersionReport,
    which: VersionTool,
) -> Vec<(&'static str, String, usize, String)> {
    tool(report, which)
        .evidence
        .iter()
        .map(|e| (e.source, e.file.clone(), e.line, e.value.clone()))
        .collect()
}

const WORKFLOW: &str = "name: ci
on: [push]
jobs:
  web:
    runs-on: ubuntu-latest
    strategy:
      matrix:
        node: [22, 24]
    steps:
      - uses: actions/checkout@v5
      - uses: pnpm/action-setup@v4
        with:
          version: 9.15.9
      - uses: actions/setup-node@v5
        with:
          node-version: ${{ matrix.node }}
      - uses: actions/setup-node@v5
        with:
          node-version: 22.14
  api:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/setup-go@v6
        with:
          go-version: '1.25'
      - uses: actions/setup-python@v6
        with:
          python-version: 3.10
      - uses: dtolnay/rust-toolchain@1.80.0
      - uses: dtolnay/rust-toolchain@master
        with:
          toolchain: 1.81.0
";

fn src(
    source: &'static str,
    file: &str,
    line: usize,
    value: &str,
) -> (&'static str, String, usize, String) {
    (source, file.to_string(), line, value.to_string())
}

#[test]
fn evidence_carries_its_file_and_line() {
    let report = detect(&[
        ("go.mod", "module x\n"),
        (
            "package.json",
            "{\n  \"name\": \"x\",\n  \"engines\": {\n    \"pnpm\": \">=9\",\n    \"node\": \">=22\"\n  }\n}\n",
        ),
        ("pnpm-lock.yaml", ""),
        (
            "Cargo.toml",
            "[workspace]\nmembers = []\n\n[workspace.package]\nrust-version = \"1.85\"\n",
        ),
        (
            "pyproject.toml",
            "[project]\nname = \"x\"\nrequires-python = \">=3.12\"\n",
        ),
        (
            ".tool-versions",
            "# tools\ngolang 1.25.4\nnodejs 22.14.0 20.0.0\npnpm 9.15.0\nrust 1.82.0\npython 3.12.1 # main\nruby 3.3.0\n",
        ),
        (".github/workflows/ci.yml", WORKFLOW),
        (
            "Dockerfile",
            "ARG V=1\nFROM node:22-alpine AS web\nFROM golang:1.24.3-bookworm\nFROM --platform=$X python:3.12 AS b\nFROM rust:$V\nfrom registry.local:5000/library/node:24.1.0@sha256:abc\nFROM python\n",
        ),
        ("api.Dockerfile", "FROM golang:1.26\n"),
    ]);
    let wf = ".github/workflows/ci.yml";
    assert_eq!(
        evidence_of(&report, VersionTool::Go),
        [
            src("tool_versions", ".tool-versions", 2, "1.25.4"),
            src("github_actions", wf, 25, "1.25"),
            src("dockerfile", "Dockerfile", 3, "1.24.3"),
            src("dockerfile", "api.Dockerfile", 1, "1.26"),
        ]
    );
    assert_eq!(
        evidence_of(&report, VersionTool::Node),
        [
            src("tool_versions", ".tool-versions", 3, "22.14.0"),
            src("package_json_engines", "package.json", 5, ">=22"),
            src("github_actions", wf, 19, "22.14"),
            src("dockerfile", "Dockerfile", 2, "22"),
            src("dockerfile", "Dockerfile", 6, "24.1.0"),
        ]
    );
    assert_eq!(
        evidence_of(&report, VersionTool::Pnpm),
        [
            src("tool_versions", ".tool-versions", 4, "9.15.0"),
            src("package_json_engines", "package.json", 4, ">=9"),
            src("github_actions", wf, 13, "9.15.9"),
        ]
    );
    assert_eq!(
        evidence_of(&report, VersionTool::Rust),
        [
            src("cargo_rust_version", "Cargo.toml", 5, "1.85"),
            src("tool_versions", ".tool-versions", 5, "1.82.0"),
            src("github_actions", wf, 29, "1.80.0"),
            src("github_actions", wf, 32, "1.81.0"),
        ]
    );
    assert_eq!(
        evidence_of(&report, VersionTool::Python),
        [
            src("pyproject_requires_python", "pyproject.toml", 3, ">=3.12"),
            src("tool_versions", ".tool-versions", 6, "3.12.1"),
            src("github_actions", wf, 28, "3.10"),
            src("dockerfile", "Dockerfile", 4, "3.12"),
        ]
    );
}

#[test]
fn package_rust_version_wins_over_the_workspace_one() {
    let report = detect(&[(
        "Cargo.toml",
        "[package]\nname = \"x\"\nrust-version = \"1.80.0\"\n\n[workspace.package]\nrust-version = \"1.70.0\"\n",
    )]);
    assert_eq!(
        evidence_of(&report, VersionTool::Rust),
        [src("cargo_rust_version", "Cargo.toml", 3, "1.80.0")]
    );
}

#[test]
fn evidence_that_disagrees_with_the_pin_is_drift() {
    let report = detect(&[
        ("go.mod", "module x\n\ngo 1.26.1\n"),
        (
            "Dockerfile",
            "FROM golang:1.24-alpine\nFROM golang:1.26\nFROM golang:1.26.1\n",
        ),
        (".tool-versions", "golang 1.26.2\n"),
    ]);
    let go = tool(&report, VersionTool::Go);
    assert_eq!(go.status, VersionStatus::Pinned);
    let drift: Vec<(&str, &str, usize, &str, &str)> = go
        .drift
        .iter()
        .map(|d| {
            (
                d.source,
                d.file.as_str(),
                d.line,
                d.value.as_str(),
                d.pinned.as_str(),
            )
        })
        .collect();
    assert_eq!(
        drift,
        [
            ("tool_versions", ".tool-versions", 1, "1.26.2", "1.26.1"),
            ("dockerfile", "Dockerfile", 1, "1.24", "1.26.1"),
        ]
    );
    assert_eq!(go.proposal.action, ProposalAction::None);
}

#[test]
fn unreadable_files_are_skipped_with_a_reason() {
    let mut files = RepoFiles::new();
    files.add("go.mod", b"module x\ngo 1.26.1\n".to_vec());
    files.add(".github/workflows/ci.yml", b"jobs: [unclosed\n".to_vec());
    files.add(".github/workflows/ok.yaml", b"jobs: {}\n".to_vec());
    files.add("package.json", vec![0xff, 0xfe]);
    files.add("Dockerfile", vec![b'#'; versions::MAX_FILE_BYTES + 1]);
    let report = block_on(versions::detect(&files, &FakeReleases::new()));
    let skipped: Vec<(&str, &str)> = report
        .files_skipped
        .iter()
        .map(|s| (s.path.as_str(), s.reason))
        .collect();
    assert_eq!(
        skipped,
        [
            ("package.json", "not_utf8"),
            ("Dockerfile", "too_large"),
            (".github/workflows/ci.yml", "yaml_invalid"),
        ]
    );
    assert!(
        report
            .toolchains
            .iter()
            .any(|t| t.tool == VersionTool::Node)
    );
}

#[test]
fn only_the_first_sixty_four_workflows_are_read() {
    let mut files = RepoFiles::new();
    files.add("go.mod", b"module x\n".to_vec());
    for n in 0..=versions::MAX_WORKFLOWS {
        let body = format!(
            "jobs:\n  a:\n    steps:\n      - uses: actions/setup-go@v6\n        with:\n          go-version: 1.{n}.0\n"
        );
        files.add(&format!(".github/workflows/w{n:03}.yml"), body.into_bytes());
    }
    let report = block_on(versions::detect(&files, &FakeReleases::new()));
    assert_eq!(
        tool(&report, VersionTool::Go).evidence.len(),
        versions::MAX_WORKFLOWS
    );
    assert_eq!(report.files_skipped.len(), 1);
    assert_eq!(report.files_skipped[0].path, ".github/workflows/w064.yml");
    assert_eq!(report.files_skipped[0].reason, "workflow_limit");
}

#[test]
fn file_use_names_the_files_the_caller_collects() {
    for path in [
        "go.mod",
        "package.json",
        ".nvmrc",
        "Dockerfile",
        "web.Dockerfile",
        ".github/workflows/ci.yml",
        ".github/workflows/a.yaml",
        ".saka/environment.toml",
        "pyproject.toml",
    ] {
        assert_eq!(versions::file_use(path), Some(FileUse::Content), "{path}");
    }
    for path in [
        "pnpm-lock.yaml",
        "Cargo.lock",
        "bun.lockb",
        "requirements.txt",
        "go.sum",
    ] {
        assert_eq!(versions::file_use(path), Some(FileUse::Presence), "{path}");
    }
    for path in [
        "web/Dockerfile",
        "web/package.json",
        ".github/workflows/nested/ci.yml",
        ".github/workflows/.yml",
        "README.md",
        ".Dockerfile",
    ] {
        assert_eq!(versions::file_use(path), None, "{path}");
    }
}

fn resolved(
    which: VersionTool,
    files: &[(&str, &str)],
    index: &FakeReleases,
) -> Result<String, String> {
    let report = detect_with(files, index);
    let proposal = &tool(&report, which).proposal;
    match (&proposal.version, proposal.code) {
        (Some(v), _) => Ok(v.clone()),
        (None, Some("version_unresolved")) => {
            Err(proposal.params["reason"].as_str().unwrap().to_string())
        }
        other => panic!("unexpected proposal {other:?}"),
    }
}

fn node_hint(hint: &str, index: &FakeReleases) -> Result<String, String> {
    resolved(
        VersionTool::Node,
        &[("package.json", "{}"), (".nvmrc", hint)],
        index,
    )
}

#[test]
fn hints_resolve_to_the_newest_matching_release() {
    let index = node_index();
    for (hint, want) in [
        ("22", "22.15.1"),
        ("22.x", "22.15.1"),
        ("22.*", "22.15.1"),
        ("v22", "22.15.1"),
        ("22.14", "22.14.2"),
        ("22.14.x", "22.14.2"),
        ("^22.1.0", "22.15.1"),
        ("~22.14.0", "22.14.2"),
        ("^22.15.2", "no_matching_release"),
        ("lts/*", "24.11.0"),
        ("lts/jod", "22.15.1"),
        ("LTS/Iron", "20.19.0"),
    ] {
        let got = node_hint(hint, &index);
        assert_eq!(got.unwrap_or_else(|e| e), want, "{hint}");
    }
}

#[test]
fn ranges_and_names_are_never_guessed() {
    let index = node_index();
    for hint in [
        ">=22",
        "latest",
        "22 || 24",
        "node",
        "stable",
        "*",
        "<23",
        "22.14.0 - 22.15.0",
    ] {
        assert_eq!(
            node_hint(hint, &index),
            Err("unsupported_hint".into()),
            "{hint}"
        );
    }
}

#[test]
fn exact_hints_need_no_index() {
    let index = node_index().failing(Tool::Node);
    let got = resolved(
        VersionTool::Node,
        &[
            ("package.json", "{}"),
            (".nvmrc", "latest\n"),
            ("Dockerfile", "FROM node:v22.14.0\n"),
        ],
        &index,
    );
    assert_eq!(got, Ok("22.14.0".into()));
    assert!(index.calls().is_empty());
}

#[test]
fn the_first_resolvable_hint_wins_in_table_order() {
    let report = detect(&[
        ("package.json", r#"{"engines":{"node":">=20"}}"#),
        (".nvmrc", "latest\n"),
        ("Dockerfile", "FROM node:24-slim\nFROM node:22\n"),
    ]);
    let node = tool(&report, VersionTool::Node);
    assert_eq!(node.proposal.version.as_deref(), Some("24.11.0"));
}

#[test]
fn an_unreachable_index_leaves_the_version_unresolved() {
    let index = node_index().failing(Tool::Node);
    assert_eq!(node_hint("22", &index), Err("index_unavailable".into()));
    let report = detect_with(&[("package.json", "{}"), (".nvmrc", "22\n")], &index);
    let proposal = &tool(&report, VersionTool::Node).proposal;
    assert_eq!(proposal.action, ProposalAction::Manual);
    assert_eq!(
        proposal.params,
        json!({ "tool": "node", "hints": ["22"], "reason": "index_unavailable" })
    );
}

#[test]
fn prereleases_and_unparsable_versions_never_win() {
    let report = detect(&[
        ("package.json", r#"{"packageManager":"pnpm@9"}"#),
        (".nvmrc", "22.14.0\n"),
    ]);
    assert_eq!(
        tool(&report, VersionTool::Pnpm).proposal.version.as_deref(),
        Some("9.15.9")
    );
    let report = detect(&[
        ("package.json", r#"{"packageManager":"pnpm@10"}"#),
        (".nvmrc", "22.14.0\n"),
    ]);
    assert_eq!(
        tool(&report, VersionTool::Pnpm).proposal.params["reason"],
        "no_matching_release"
    );
}

#[test]
fn go_resolves_modern_and_legacy_release_names() {
    let index = node_index();
    let go = |hint: &str| {
        resolved(
            VersionTool::Go,
            &[
                ("go.mod", "module x\n"),
                ("Dockerfile", &format!("FROM golang:{hint}\n")),
            ],
            &index,
        )
    };
    assert_eq!(go("1.24"), Ok("1.24.10".into()));
    assert_eq!(go("1.20"), Ok("1.20.14".into()));
    assert_eq!(go("go1.25"), Ok("1.25.4".into()));
}

#[test]
fn rust_and_python_only_take_exact_hints() {
    let index = FakeReleases::new();
    let rust = |rv: &str| {
        resolved(
            VersionTool::Rust,
            &[(
                "Cargo.toml",
                &format!("[package]\nname = \"x\"\nrust-version = \"{rv}\"\n"),
            )],
            &index,
        )
    };
    assert_eq!(rust("1.80"), Err("no_release_index".into()));
    assert_eq!(rust("1.80.0"), Ok("1.80.0".into()));
    let python = resolved(
        VersionTool::Python,
        &[
            (
                "pyproject.toml",
                "[project]\nrequires-python = \">=3.12\"\n",
            ),
            (".python-version", "3.12\n"),
        ],
        &index,
    );
    assert_eq!(python, Err("no_release_index".into()));
    assert!(index.calls().is_empty());
    let none = resolved(
        VersionTool::Rust,
        &[("Cargo.toml", "[package]\nname = \"x\"\n")],
        &index,
    );
    assert_eq!(none, Err("no_hints".into()));
}

struct Expect {
    action: ProposalAction,
    path: Option<&'static str>,
    content: Option<&'static str>,
    command: Option<&'static str>,
    code: Option<&'static str>,
}

fn file(action: ProposalAction, path: &'static str, content: &'static str) -> Expect {
    Expect {
        action,
        path: Some(path),
        content: Some(content),
        command: None,
        code: None,
    }
}

fn manual(code: &'static str, command: Option<&'static str>) -> Expect {
    Expect {
        action: ProposalAction::Manual,
        path: None,
        content: None,
        command,
        code: Some(code),
    }
}

#[test]
fn every_proposal_row_gives_its_action_path_and_content_or_command() {
    use ProposalAction::{None as Nothing, ReplaceFile, WriteFile};
    let pkg = ("package.json", "{}");
    let cargo = ("Cargo.toml", "[package]\nname = \"x\"\n");
    let cases: Vec<(VersionTool, Repo, Expect)> = vec![
        (
            VersionTool::Node,
            vec![pkg, (".nvmrc", "22.14.0\n")],
            Expect {
                action: Nothing,
                path: None,
                content: None,
                command: None,
                code: None,
            },
        ),
        (
            VersionTool::Node,
            vec![pkg, (".nvmrc", "22\n")],
            file(ReplaceFile, ".nvmrc", "22.15.1\n"),
        ),
        (
            VersionTool::Node,
            vec![pkg, (".node-version", "lts/*\n")],
            file(ReplaceFile, ".node-version", "24.11.0\n"),
        ),
        (
            VersionTool::Node,
            vec![(
                "package.json",
                r#"{"devEngines":{"runtime":{"name":"node","version":"22"}}}"#,
            )],
            manual(
                "set_dev_engines_runtime",
                Some("npm pkg set devEngines.runtime.name=node devEngines.runtime.version=22.15.1"),
            ),
        ),
        (
            VersionTool::Node,
            vec![pkg, ("Dockerfile", "FROM node:22.14\n")],
            file(WriteFile, ".nvmrc", "22.14.2\n"),
        ),
        (
            VersionTool::Node,
            vec![pkg, (".nvmrc", "\n"), ("Dockerfile", "FROM node:22.14\n")],
            file(ReplaceFile, ".nvmrc", "22.14.2\n"),
        ),
        (
            VersionTool::Pnpm,
            vec![("package.json", r#"{"packageManager":"pnpm@9"}"#)],
            manual(
                "set_package_manager",
                Some("npm pkg set packageManager=pnpm@9.15.9"),
            ),
        ),
        (
            VersionTool::Pnpm,
            vec![
                pkg,
                ("pnpm-lock.yaml", ""),
                (".tool-versions", "pnpm 9.15.0\n"),
            ],
            manual(
                "set_package_manager",
                Some("npm pkg set packageManager=pnpm@9.15.0"),
            ),
        ),
        (
            VersionTool::Pnpm,
            vec![(
                "package.json",
                r#"{"devEngines":{"packageManager":{"name":"pnpm","version":"^9.15.0"}}}"#,
            )],
            manual(
                "set_dev_engines_package_manager",
                Some("npm pkg set devEngines.packageManager.version=9.15.9"),
            ),
        ),
        (
            VersionTool::Go,
            vec![
                ("go.mod", "module x\n"),
                ("Dockerfile", "FROM golang:1.25\n"),
            ],
            manual("set_go_directive", Some("go mod edit -go=1.25.4")),
        ),
        (
            VersionTool::Go,
            vec![
                ("go.mod", "module x\ngo 1.26\ntoolchain default\n"),
                (".tool-versions", "golang 1.26.2\n"),
            ],
            manual("set_go_toolchain", Some("go mod edit -toolchain=go1.26.2")),
        ),
        (
            VersionTool::Rust,
            vec![cargo, (".tool-versions", "rust 1.82.0\n")],
            file(
                WriteFile,
                "rust-toolchain.toml",
                "[toolchain]\nchannel = \"1.82.0\"\n",
            ),
        ),
        (
            VersionTool::Rust,
            vec![
                cargo,
                ("rust-toolchain.toml", "[toolchain]\nchannel = \"stable\"\n"),
                (".tool-versions", "rust 1.82.0\n"),
            ],
            manual("set_rust_channel", None),
        ),
        (
            VersionTool::Rust,
            vec![
                cargo,
                ("rust-toolchain", "stable\n"),
                (".tool-versions", "rust 1.82.0\n"),
            ],
            file(ReplaceFile, "rust-toolchain", "1.82.0\n"),
        ),
        (
            VersionTool::Python,
            vec![
                (".python-version", "3.12\n"),
                (".tool-versions", "python 3.12.7\n"),
            ],
            file(ReplaceFile, ".python-version", "3.12.7\n"),
        ),
        (
            VersionTool::Python,
            vec![
                ("requirements.txt", ""),
                ("Dockerfile", "FROM python:3.13.1-slim\n"),
            ],
            file(WriteFile, ".python-version", "3.13.1\n"),
        ),
        (
            VersionTool::Python,
            vec![("setup.py", "")],
            manual("version_unresolved", None),
        ),
    ];
    for (which, files, want) in cases {
        let report = detect(&files);
        let p = &tool(&report, which).proposal;
        assert_eq!(p.action, want.action, "{files:?}");
        assert_eq!(p.path.as_deref(), want.path, "{files:?}");
        assert_eq!(p.content.as_deref(), want.content, "{files:?}");
        assert_eq!(p.command.as_deref(), want.command, "{files:?}");
        assert_eq!(p.code, want.code, "{files:?}");
        assert!(!p.written);
        if p.action == ProposalAction::Manual {
            assert!(p.content.is_none() && p.path.is_none(), "{files:?}");
        }
    }
    let report = detect(&[
        cargo,
        ("rust-toolchain.toml", "[toolchain]\nchannel = \"stable\"\n"),
        (".tool-versions", "rust 1.82.0\n"),
    ]);
    assert_eq!(
        tool(&report, VersionTool::Rust).proposal.params,
        json!({ "path": "rust-toolchain.toml", "channel": "1.82.0" })
    );
}

#[test]
fn only_go_node_and_pnpm_are_provisioned_by_saka() {
    let report = detect(&[
        ("go.mod", "module x\ngo 1.26.1\n"),
        ("package.json", "{}"),
        ("pnpm-lock.yaml", ""),
        ("Cargo.toml", ""),
        ("pyproject.toml", ""),
    ]);
    let provisioned: Vec<(VersionTool, bool)> = report
        .toolchains
        .iter()
        .map(|t| (t.tool, t.provisioned_by_saka))
        .collect();
    assert_eq!(
        provisioned,
        [
            (VersionTool::Go, true),
            (VersionTool::Node, true),
            (VersionTool::Pnpm, true),
            (VersionTool::Rust, false),
            (VersionTool::Python, false),
        ]
    );
}

#[test]
fn fixture_repos_report_their_toolchains() {
    let index = node_index();
    let node = block_on(versions::detect(&load_fixture("node"), &index));
    assert_eq!(
        summary(tool(&node, VersionTool::Node)),
        (VersionStatus::Unpinned, None, Some("nvmrc"))
    );
    assert_eq!(
        tool(&node, VersionTool::Node).proposal.content.as_deref(),
        Some("22.15.1\n")
    );
    assert_eq!(
        tool(&node, VersionTool::Pnpm).status,
        VersionStatus::Undeclared
    );
    assert_eq!(
        tool(&node, VersionTool::Pnpm).proposal.params["reason"],
        "no_hints"
    );

    let go = block_on(versions::detect(&load_fixture("go"), &index));
    assert_eq!(tool(&go, VersionTool::Go).drift.len(), 1);

    let rust = block_on(versions::detect(&load_fixture("rust"), &index));
    let r = tool(&rust, VersionTool::Rust);
    assert_eq!(r.status, VersionStatus::EvidenceOnly);
    assert_eq!(r.proposal.path.as_deref(), Some("rust-toolchain.toml"));

    let python = block_on(versions::detect(&load_fixture("python"), &index));
    let p = tool(&python, VersionTool::Python);
    assert_eq!(
        evidence_of(&python, VersionTool::Python),
        [
            src("pyproject_requires_python", "pyproject.toml", 3, ">=3.12"),
            src("github_actions", ".github/workflows/ci.yml", 10, "3.12.7"),
        ]
    );
    assert_eq!(p.proposal.content.as_deref(), Some("3.12.7\n"));
}

#[test]
fn the_report_serializes_with_snake_case_vocabularies() {
    let report = detect(&[("package.json", "{}"), (".nvmrc", "22\n")]);
    let value = serde_json::to_value(&report).unwrap();
    assert_eq!(value["toolchains"][0]["tool"], "node");
    assert_eq!(value["toolchains"][0]["status"], "unpinned");
    assert_eq!(value["toolchains"][0]["proposal"]["action"], "replace_file");
    assert_eq!(value["files_skipped"], json!([]));
}
