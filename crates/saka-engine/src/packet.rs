use std::fmt::Write as _;

use saka_core::model::{ContextRef, DraftDocument};
use saka_core::vocab::{
    ContextOrigin, DocsMode, DraftLocale, DraftMode, PacketOrigin, TargetKind, Tool, WorkKind,
};

use saka_spec::content_hash;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PacketStep {
    pub task_key: String,
    pub work: String,
    pub criteria: Vec<(String, String)>,
    pub commands: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PacketSpec {
    pub revision_id: String,
    pub path: String,
    pub source_commit: String,
    pub content: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PacketRepo {
    pub name: String,
    pub full_name: String,
    pub base_branch: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PacketDocs {
    pub path: String,
    pub full_name: String,
    pub project_path: String,
    pub commit: String,
    pub mode: DocsMode,
    pub branch: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EnvironmentFacts {
    pub repo: Option<PacketRepo>,
    pub path_scope: Vec<String>,
    pub docs: Option<PacketDocs>,
    pub ports: Option<(u16, u16)>,
    pub toolchains: Vec<(Tool, String)>,
    pub session_secrets: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PacketRegression {
    pub commands: Vec<String>,
    pub test_paths: Vec<String>,
    pub proven: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PacketInput {
    pub step: PacketStep,
    pub regression: Option<PacketRegression>,
    pub spec: PacketSpec,
    pub context: Vec<(ContextRef, Vec<u8>)>,
    pub environment: EnvironmentFacts,
    pub retry: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PacketDocument {
    pub index: u32,
    pub path: String,
    pub origin: Option<ContextOrigin>,
    pub bytes: u64,
    pub content_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Packet {
    pub text: String,
    pub source_commit: String,
    pub documents: Vec<PacketDocument>,
    pub sha256: String,
}

fn push_document(out: &mut String, index: u32, path: &str, commit: &str, content: &[u8]) {
    push_source(out, index, &format!("{path} @ {commit}"), content);
}

fn push_source(out: &mut String, index: u32, source: &str, content: &[u8]) {
    let text = String::from_utf8_lossy(content).replace("</document", "<\\/document");
    let _ = write!(
        out,
        "<document index=\"{index}\">\n<source>{source}</source>\n<document_content>\n{}\n</document_content>\n</document>\n",
        text.trim_end_matches('\n')
    );
}

// Content is escaped at `</document`, so each document ends at the first `</document>` after it.
#[must_use]
pub fn sources(text: &str) -> std::collections::BTreeMap<u32, String> {
    let mut out = std::collections::BTreeMap::new();
    let Some(mut rest) = text.strip_prefix("<documents>\n") else {
        return out;
    };
    while let Some(document) = rest.strip_prefix("<document index=\"") {
        let Some((index, body)) = document.split_once("\">\n<source>") else {
            break;
        };
        let Some((source, _)) = body.split_once("</source>\n") else {
            break;
        };
        if let Ok(index) = index.parse() {
            out.insert(index, source.to_string());
        }
        let Some((_, next)) = body.split_once("</document>\n") else {
            break;
        };
        rest = next;
    }
    out
}

fn push_regression(out: &mut String, regression: &PacketRegression) {
    out.push_str(
        "\nThis step fixes a bug. Regression commands (they must pass when you finish):\n",
    );
    for command in &regression.commands {
        let _ = writeln!(out, "- `{command}`");
    }
    if regression.proven {
        out.push_str(
            "Saka already proved your regression test: the commands fail without the fix. Keep that test committed and unchanged, and make the commands pass.\n",
        );
        return;
    }
    let _ = write!(
        out,
        "\nWork test first:\n\
         1. Write a regression test that fails on the current code because of the bug. Change test files only: a file counts as a test when its path matches one of {}.\n\
         2. Commit it with a Conventional Commits message, and leave the worktree clean.\n\
         3. Call the `regression_check` tool (`mcp__saka__regression_check`), then end your turn. Saka runs the regression commands at that commit, and they must fail there. The result arrives as your next message. You get three checks.\n\
         4. Fix the bug only after the result says reproduced. Then make the regression commands and the Done when commands pass, and commit. Do not push.\n",
        regression.test_paths.join(", ")
    );
}

fn push_docs_line(out: &mut String, docs: &PacketDocs) {
    match (docs.mode, docs.branch.as_deref()) {
        (DocsMode::Writable, Some(branch)) => {
            let _ = writeln!(
                out,
                "- Project docs: {}, a checkout of {} on branch {branch}. Edit docs only under {}/ there and commit them in that checkout; Saka opens a separate docs pull request after the code pull request.",
                docs.path, docs.full_name, docs.project_path
            );
        }
        _ => {
            let commit: String = docs.commit.chars().take(12).collect();
            let folder = if docs.project_path == "." {
                docs.full_name.clone()
            } else {
                format!("{}/{}", docs.full_name, docs.project_path)
            };
            let _ = writeln!(
                out,
                "- Project docs (read-only): {} ({folder} at {commit})",
                docs.path
            );
        }
    }
}

fn listed(items: &[String]) -> String {
    if items.is_empty() {
        "none".to_string()
    } else {
        items.join(", ")
    }
}

#[must_use]
pub fn render(input: &PacketInput) -> Packet {
    let spec = &input.spec;
    let mut text = String::from("<documents>\n");
    push_document(&mut text, 1, &spec.path, &spec.source_commit, &spec.content);
    let mut documents = vec![PacketDocument {
        index: 1,
        path: spec.path.clone(),
        origin: None,
        bytes: spec.content.len() as u64,
        content_hash: content_hash(&spec.content),
    }];
    for (index, (doc, content)) in (2u32..).zip(&input.context) {
        push_document(&mut text, index, &doc.path, &spec.source_commit, content);
        documents.push(PacketDocument {
            index,
            path: doc.path.clone(),
            origin: Some(doc.origin),
            bytes: doc.bytes,
            content_hash: doc.content_hash.clone(),
        });
    }
    text.push_str("</documents>\n\n");

    let _ = write!(
        text,
        "You are implementing one step of an approved spec in this git repository.\n\
         Document 1 is the spec (pinned revision {}).",
        spec.revision_id
    );
    match input.context.len() {
        0 => {}
        1 => text.push_str(
            " Document 2 is a doc it links to or that the project marks as always relevant, read at the same commit.",
        ),
        n => {
            let _ = write!(
                text,
                " Documents 2–{} are the docs it links to and the docs the project marks as always relevant, read at the same commit.",
                n + 1
            );
        }
    }
    text.push_str(" Use the documents as context; the step below defines the work.\n\n");

    let step = &input.step;
    let _ = writeln!(text, "Task: {} — {}", step.task_key, step.work);
    text.push_str("Acceptance criteria for this step:\n");
    for (id, criterion) in &step.criteria {
        let _ = writeln!(text, "- {id}: {criterion}");
    }
    text.push_str("Done when (run these and make them pass before you finish):\n");
    for command in &step.commands {
        let _ = writeln!(text, "- `{command}`");
    }
    if let Some(regression) = &input.regression {
        push_regression(&mut text, regression);
    }

    let env = &input.environment;
    let ports = match env.ports {
        Some((base, count)) => format!(
            "{base}–{} (SAKA_PORT_BASE, SAKA_PORT_COUNT, PORT)",
            u32::from(base) + u32::from(count) - 1
        ),
        None => "none".to_string(),
    };
    let mut toolchains = env.toolchains.clone();
    toolchains.sort();
    let toolchains: Vec<String> = toolchains
        .iter()
        .map(|(tool, version)| format!("{} {version}", tool.as_str()))
        .collect();
    let mut secrets = env.session_secrets.clone();
    secrets.sort();
    text.push_str("\nEnvironment:\n");
    if let Some(repo) = &env.repo {
        let everything = saka_core::scope::PathScope::parse(&env.path_scope)
            .is_ok_and(|scope| scope.allows_everything());
        let paths = if everything {
            "all".to_string()
        } else {
            env.path_scope.join(", ")
        };
        let _ = write!(
            text,
            "- Repository: {} ({}), base branch {}\n- Paths you may change: {paths}\n",
            repo.name, repo.full_name, repo.base_branch
        );
    }
    if let Some(docs) = &env.docs {
        push_docs_line(&mut text, docs);
    }
    let _ = write!(
        text,
        "- Ports: {ports}\n- Toolchains: {}\n- Secrets available as environment variables: {}\n\n",
        listed(&toolchains),
        listed(&secrets)
    );
    if let Some(retry) = &input.retry {
        let _ = write!(text, "{retry}\n\n");
    }
    text.push_str(
        "Rules:\n\
         - Work only inside this repository, and only in the paths you may change.\n",
    );
    if env
        .docs
        .as_ref()
        .is_some_and(|d| d.mode == DocsMode::Writable)
    {
        text.push_str("- Commit docs changes only in the project docs checkout.\n");
    }
    text.push_str(
        "- Commit all your changes with a Conventional Commits message before you finish.\n\
         - Do not push. Saka pushes the branch and opens the pull request.\n\
         - If the spec and the docs leave open a choice that only the operator can make (a data store, a public interface, a dependency, anything costly to undo), call the `request_decision` tool (`mcp__saka__request_decision`) with two or three options and your recommendation first, then end your turn. The operator's answer arrives as your next message. Never ask what the spec, the docs, or the code already answer. One question waits at a time, and a step may ask five at most.\n",
    );
    let sha256 = content_hash(text.as_bytes());
    Packet {
        text,
        source_commit: spec.source_commit.clone(),
        documents,
        sha256,
    }
}

pub const ARCHITECT_MAX_DOCUMENTS: usize = 32;
pub const ARCHITECT_MAX_BYTES: u64 = 256 * 1024;
pub const ARCHITECT_MAX_INDEX: usize = 500;
pub const ARCHITECT_MAX_ISSUES: usize = 50;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviousAttempt {
    pub attempt: u32,
    pub code: String,
    pub seeded: bool,
    pub issues: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchitectInput {
    pub project_name: String,
    pub project_path: String,
    pub base_sha: String,
    pub locale: DraftLocale,
    pub mode: DraftMode,
    pub target_kind: TargetKind,
    pub kind: WorkKind,
    pub idea: String,
    pub target: String,
    pub selected: Vec<(String, Vec<u8>)>,
    pub always: Vec<(String, Vec<u8>)>,
    pub unreadable: Vec<String>,
    pub index: Vec<String>,
    pub previous: Option<PreviousAttempt>,
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchitectPacket {
    pub text: String,
    pub sha256: String,
    pub documents: Vec<DraftDocument>,
}

fn template_locale(locale: DraftLocale) -> saka_spec::Locale {
    match locale {
        DraftLocale::En => saka_spec::Locale::En,
        DraftLocale::Id => saka_spec::Locale::Id,
    }
}

fn language(locale: DraftLocale) -> &'static str {
    match locale {
        DraftLocale::En => "English",
        DraftLocale::Id => "Indonesian (Bahasa Indonesia)",
    }
}

#[must_use]
pub fn issue_line(issue: &serde_json::Value) -> String {
    let code = issue
        .get("code")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let line = issue
        .get("line")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(1);
    let params = issue
        .get("params")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    format!("- line {line}: {code} {params}")
}

#[must_use]
pub fn blocking_issues(issues: &[serde_json::Value]) -> Vec<&serde_json::Value> {
    issues
        .iter()
        .filter(|i| {
            i.get("severity").and_then(serde_json::Value::as_str) == Some("error")
                && i.get("code").and_then(serde_json::Value::as_str)
                    != Some(saka_spec::LintCode::DecisionUnanswered.as_str())
        })
        .collect()
}

#[must_use]
pub fn issue_list(issues: &[&serde_json::Value]) -> String {
    let mut out = String::new();
    for issue in issues.iter().take(ARCHITECT_MAX_ISSUES) {
        let _ = writeln!(out, "{}", issue_line(issue));
    }
    if issues.len() > ARCHITECT_MAX_ISSUES {
        let _ = writeln!(out, "- …and {} more", issues.len() - ARCHITECT_MAX_ISSUES);
    }
    out
}

#[must_use]
pub fn render_architect(input: &ArchitectInput) -> ArchitectPacket {
    let mut text = String::from("<documents>\n");
    let mut documents = Vec::new();
    let template = saka_spec::template(template_locale(input.locale), input.kind).as_bytes();
    let rules = saka_spec::rules().as_bytes();
    let fixed = [
        (
            format!(
                "saka:spec-template/{} (parser {})",
                input.locale.as_str(),
                saka_spec::PARSER_VERSION
            ),
            PacketOrigin::Template,
            template,
        ),
        (
            format!("saka:spec-rules (parser {})", saka_spec::PARSER_VERSION),
            PacketOrigin::Rules,
            rules,
        ),
    ];
    for (index, (source, origin, content)) in (1u32..).zip(fixed) {
        push_source(&mut text, index, &source, content);
        documents.push(DraftDocument {
            index,
            path: None,
            origin,
            bytes: content.len() as u64,
            content_hash: content_hash(content),
        });
    }
    let mut seen: Vec<&str> = vec![input.target.as_str()];
    let mut wanted: Vec<(&str, PacketOrigin, &[u8])> = Vec::new();
    for (path, content) in &input.selected {
        if !seen.contains(&path.as_str()) {
            seen.push(path);
            wanted.push((path, PacketOrigin::Selected, content));
        }
    }
    let mut always: Vec<&(String, Vec<u8>)> = input.always.iter().collect();
    always.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    for (path, content) in always {
        if !seen.contains(&path.as_str()) {
            seen.push(path);
            wanted.push((path, PacketOrigin::Always, content));
        }
    }
    let mut omitted: Vec<&str> = Vec::new();
    let mut total = 0u64;
    let mut index = 3u32;
    for (path, origin, content) in wanted {
        let bytes = content.len() as u64;
        let fits = omitted.is_empty()
            && documents.len() - 2 < ARCHITECT_MAX_DOCUMENTS
            && total + bytes <= ARCHITECT_MAX_BYTES;
        if !fits {
            omitted.push(path);
            continue;
        }
        total += bytes;
        push_source(
            &mut text,
            index,
            &format!("{path} @ {}", input.base_sha),
            content,
        );
        documents.push(DraftDocument {
            index,
            path: Some(path.to_string()),
            origin,
            bytes,
            content_hash: content_hash(content),
        });
        index += 1;
    }
    omitted.extend(input.unreadable.iter().map(String::as_str));
    text.push_str("</documents>\n\n");

    let conversation = input.mode == DraftMode::Session;
    let revising = input.target_kind == TargetKind::Revise;
    let _ = writeln!(
        text,
        "You are the architect for project \"{}\". {} from the idea below.{}",
        input.project_name,
        if revising {
            "Revise the existing spec in this repository"
        } else {
            "Write one new spec in this repository"
        },
        if conversation {
            " The operator reads your work after every turn and replies, so this is a conversation."
        } else {
            ""
        }
    );
    text.push_str(
        "Document 1 is the spec template and document 2 is the spec format and its lint rules.",
    );
    match documents.len() - 2 {
        0 => {}
        1 => {
            let _ = write!(
                text,
                " Document 3 is a doc the operator chose or the project marks as always relevant, read at commit {}.",
                input.base_sha
            );
        }
        n => {
            let _ = write!(
                text,
                " Documents 3–{} are docs the operator chose and docs the project marks as always relevant, read at commit {}.",
                n + 2,
                input.base_sha
            );
        }
    }
    let _ = write!(
        text,
        "\n\nIdea:\n<idea>\n{}\n</idea>\n\nTarget file: {}{}\nLanguage: write the prose in {}. Headings and column names stay the English keywords from document 2.\n\n",
        input.idea.trim_end_matches('\n'),
        input.target,
        if revising {
            " (it already exists in your checkout; read it and edit it in place, keeping what the idea does not change)"
        } else {
            ""
        },
        language(input.locale)
    );
    let mut index: Vec<&String> = input.index.iter().collect();
    index.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
    let _ = writeln!(
        text,
        "Project docs ({} markdown files under {} at {}):",
        index.len(),
        input.project_path,
        input.base_sha
    );
    for path in index.iter().take(ARCHITECT_MAX_INDEX) {
        let _ = writeln!(text, "- {path}");
    }
    if index.len() > ARCHITECT_MAX_INDEX {
        let _ = writeln!(text, "- …and {} more", index.len() - ARCHITECT_MAX_INDEX);
    }
    if !omitted.is_empty() {
        let _ = writeln!(
            text,
            "Not included because of the context limit: {}",
            omitted.join(", ")
        );
    }
    text.push('\n');
    if let Some(previous) = &input.previous {
        let _ = write!(
            text,
            "Previous attempt {} stopped: {}.",
            previous.attempt, previous.code
        );
        if previous.seeded {
            let _ = write!(text, " {} already contains what it wrote.", input.target);
        }
        let blocking = blocking_issues(&previous.issues);
        if blocking.is_empty() {
            text.push('\n');
        } else {
            text.push_str(" Its last check found:\n");
            text.push_str(&issue_list(&blocking));
        }
        text.push('\n');
    }
    if let Some(note) = &input.note {
        let _ = write!(text, "Operator note:\n{}\n\n", note.trim_end_matches('\n'));
    }
    let writing = if conversation {
        format!(
            "- Write the spec to {target}. You may also create or edit other markdown files under {project}, for example docs the spec depends on. Every other write is refused.\n",
            target = input.target,
            project = input.project_path
        )
    } else {
        format!(
            "- Write the spec only to {target}. Every other write is refused.\n",
            target = input.target
        )
    };
    let ending = if conversation {
        "- Do not commit. End your turn when you have done what the operator asked. Saka checks the file after every turn and shows the operator the result; the operator answers with a message or commits. Keep the file complete at the end of every turn.\n"
    } else {
        "- Do not commit. When the file is complete, end your turn. Saka checks it and tells you what to fix.\n"
    };
    let _ = write!(
        text,
        "Rules:\n\
         - Read any file in this repository that helps. You cannot run commands or use the web.\n\
         {writing}\
         - Follow document 2 exactly and start from document 1.\n\
         - For each material technical decision the docs do not settle, add a question under \"## Decisions\" with two or three options, the recommended option first and marked \"(recommended)\". Leave every option unchecked; the operator answers.\n\
         - If you cannot write the spec without the operator's answer to a question about the idea itself, call the `request_decision` tool (`mcp__saka__request_decision`) with two or three options and your recommendation first, then end your turn. The answer arrives as your next message: write it into the spec as the checked option under \"## Decisions\", and carry on. Never ask what the docs or the idea already answer.\n\
         {ending}"
    );
    let sha256 = content_hash(text.as_bytes())
        .trim_start_matches("sha256:")
        .to_string();
    ArchitectPacket {
        text,
        sha256,
        documents,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input() -> PacketInput {
        PacketInput {
            step: PacketStep {
                task_key: "tasks-done/s1".into(),
                work: "Add the done command".into(),
                criteria: vec![("AC-01".into(), "`tasks done` marks a task.".into())],
                commands: vec!["cd cli && go test ./...".into()],
            },
            regression: None,
            spec: PacketSpec {
                revision_id: "rev-1".into(),
                path: "docs/specs/tasks-done.md".into(),
                source_commit: "c".repeat(40),
                content: b"# Tasks done\n".to_vec(),
            },
            context: Vec::new(),
            environment: EnvironmentFacts::default(),
            retry: None,
        }
    }

    fn doc(path: &str, content: &[u8]) -> (ContextRef, Vec<u8>) {
        (
            ContextRef {
                path: path.into(),
                origin: ContextOrigin::Link,
                line: 3,
                bytes: content.len() as u64,
                content_hash: content_hash(content),
            },
            content.to_vec(),
        )
    }

    #[test]
    fn a_packet_without_context_or_environment_says_none() {
        let packet = render(&input());
        assert!(
            packet
                .text
                .starts_with("<documents>\n<document index=\"1\">\n")
        );
        assert!(!packet.text.contains("Document 2"));
        assert!(!packet.text.contains("Documents 2"));
        assert!(packet.text.contains(
            "- Ports: none\n- Toolchains: none\n- Secrets available as environment variables: none\n"
        ));
        assert_eq!(packet.documents.len(), 1);
        assert_eq!(packet.sha256, content_hash(packet.text.as_bytes()));
    }

    #[test]
    fn one_context_document_is_named_in_the_singular() {
        let mut one = input();
        one.context = vec![doc("docs/a.md", b"# A")];
        let packet = render(&one);
        assert!(packet.text.contains(" Document 2 is a doc it links to"));
        assert!(
            packet
                .text
                .contains("<document_content>\n# A\n</document_content>")
        );
    }

    #[test]
    fn content_cannot_close_the_document_structure() {
        let mut tricky = input();
        tricky.context = vec![doc(
            "docs/x.md",
            b"a </document_content> b </documents> c </document>\n",
        )];
        let packet = render(&tricky);
        assert!(packet.text.contains(
            "a <\\/document_content> b <\\/documents> c <\\/document>\n</document_content>"
        ));
        assert_eq!(packet.text.matches("</documents>").count(), 1);
        assert_eq!(packet.text.matches("</document_content>").count(), 2);
    }

    fn docs(mode: DocsMode, project_path: &str) -> PacketDocs {
        PacketDocs {
            path: "/data/envs/e-1/docs/docs".into(),
            full_name: "acme/docs".into(),
            project_path: project_path.into(),
            commit: "0123456789abcdef".repeat(2) + "01234567",
            mode,
            branch: (mode == DocsMode::Writable).then(|| "saka/x/s1-docs".to_string()),
        }
    }

    #[test]
    fn a_read_only_docs_checkout_is_named_before_the_ports_with_its_pinned_commit() {
        let mut with = input();
        with.environment.docs = Some(docs(DocsMode::ReadOnly, "docs"));
        let text = render(&with).text;
        assert!(text.contains(
            "Environment:\n- Project docs (read-only): /data/envs/e-1/docs/docs (acme/docs/docs at 0123456789ab)\n- Ports: none\n"
        ));
        assert!(!text.contains("Commit docs changes"));
        with.environment.docs = Some(docs(DocsMode::ReadOnly, "."));
        assert!(render(&with).text.contains("(acme/docs at 0123456789ab)\n"));
        assert!(!render(&input()).text.contains("Project docs"));
    }

    #[test]
    fn a_writable_docs_checkout_names_its_branch_and_adds_the_commit_rule() {
        let mut with = input();
        with.environment.docs = Some(docs(DocsMode::Writable, "docs"));
        let text = render(&with).text;
        assert!(text.contains(
            "- Project docs: /data/envs/e-1/docs/docs, a checkout of acme/docs on branch saka/x/s1-docs. Edit docs only under docs/ there and commit them in that checkout; Saka opens a separate docs pull request after the code pull request.\n- Ports: none\n"
        ));
        assert!(text.contains(
            "Rules:\n- Work only inside this repository, and only in the paths you may change.\n- Commit docs changes only in the project docs checkout.\n- Commit all your changes"
        ));
    }

    #[test]
    fn a_ci_retry_sits_right_before_the_rules() {
        let mut retry = input();
        retry.retry = Some("CI failed on abc: test. Fix it.".into());
        let packet = render(&retry);
        assert!(
            packet
                .text
                .contains("variables: none\n\nCI failed on abc: test. Fix it.\n\nRules:\n")
        );
    }
}
