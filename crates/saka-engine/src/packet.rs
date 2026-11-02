use std::fmt::Write as _;

use saka_core::model::ContextRef;
use saka_core::vocab::{ContextOrigin, Tool};

use crate::context::sha256;

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

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EnvironmentFacts {
    pub ports: Option<(u16, u16)>,
    pub toolchains: Vec<(Tool, String)>,
    pub session_secrets: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PacketInput {
    pub step: PacketStep,
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
    let text = String::from_utf8_lossy(content).replace("</document", "<\\/document");
    let _ = write!(
        out,
        "<document index=\"{index}\">\n<source>{path} @ {commit}</source>\n<document_content>\n{}\n</document_content>\n</document>\n",
        text.trim_end_matches('\n')
    );
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
        content_hash: sha256(&spec.content),
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
    let _ = write!(
        text,
        "\nEnvironment:\n- Ports: {ports}\n- Toolchains: {}\n- Secrets available as environment variables: {}\n\n",
        listed(&toolchains),
        listed(&secrets)
    );
    if let Some(retry) = &input.retry {
        let _ = write!(text, "{retry}\n\n");
    }
    text.push_str(
        "Rules:\n\
         - Work only inside this repository.\n\
         - Commit all your changes with a Conventional Commits message before you finish.\n\
         - Do not push. Saka pushes the branch and opens the pull request.\n",
    );
    let sha256 = sha256(text.as_bytes());
    Packet {
        text,
        source_commit: spec.source_commit.clone(),
        documents,
        sha256,
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
                content_hash: sha256(content),
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
        assert_eq!(packet.sha256, sha256(packet.text.as_bytes()));
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
