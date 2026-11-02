use globset::{Glob, GlobBuilder, GlobSet, GlobSetBuilder};
use serde::{Deserialize, Serialize};

use crate::model::PermissionRule;

const METACHARACTERS: &[&str] = &[";", "&", "|", "`", "$(", ">", "<", "\n", "\r"];

pub const BUILTIN_PATH_OUTSIDE_WORKTREE: &str = "path_outside_worktree";
pub const BUILTIN_GIT_PUSH: &str = "git_push";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Effect {
    Allow,
    Deny,
    Ask,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleSpec {
    pub id: String,
    pub effect: Effect,
    pub tools: Vec<String>,
    #[serde(default)]
    pub paths: Vec<String>,
    #[serde(default)]
    pub commands: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolRequest {
    pub tool: String,
    pub command: Option<String>,
    pub path: Option<RequestPath>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestPath {
    pub path: String,
    pub inside_worktree: bool,
    pub through_symlink: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "name", rename_all = "snake_case")]
pub enum DecisionSource {
    Builtin(String),
    Rule(String),
    Project(String),
    Architect(String),
    Default,
}

impl DecisionSource {
    pub fn answer_source(&self) -> Option<String> {
        match self {
            DecisionSource::Builtin(name) => Some(format!("policy:builtin:{name}")),
            DecisionSource::Rule(id) => Some(format!("policy:{id}")),
            DecisionSource::Project(id) => Some(format!("policy:project:{id}")),
            DecisionSource::Architect(name) => Some(format!("policy:architect:{name}")),
            DecisionSource::Default => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub effect: Effect,
    pub source: DecisionSource,
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PolicyError {
    #[error("rule {rule}: invalid glob {glob:?}")]
    InvalidGlob { rule: String, glob: String },
    #[error("rule {rule}: no tools listed")]
    NoTools { rule: String },
}

#[derive(Debug, Clone)]
struct Rule {
    id: String,
    effect: Effect,
    tools: Vec<String>,
    paths: Option<GlobSet>,
    commands: Option<GlobSet>,
}

#[derive(Debug, Clone)]
pub struct Policy {
    rules: Vec<Rule>,
}

impl Policy {
    pub fn compile(specs: &[RuleSpec]) -> Result<Policy, PolicyError> {
        let mut rules = Vec::with_capacity(specs.len());
        for spec in specs {
            if spec.tools.is_empty() {
                return Err(PolicyError::NoTools {
                    rule: spec.id.clone(),
                });
            }
            rules.push(Rule {
                id: spec.id.clone(),
                effect: spec.effect,
                tools: spec.tools.clone(),
                paths: globs(&spec.id, &spec.paths, true)?,
                commands: globs(&spec.id, &spec.commands, false)?,
            });
        }
        Ok(Policy { rules })
    }

    pub fn evaluate(&self, request: &ToolRequest) -> Decision {
        self.evaluate_with(request, &[])
    }

    pub fn evaluate_with(&self, request: &ToolRequest, project: &[PermissionRule]) -> Decision {
        if let Some(name) = builtin_deny(request) {
            return Decision {
                effect: Effect::Deny,
                source: DecisionSource::Builtin(name.to_string()),
                message: None,
            };
        }
        if let Some(rule) = self.first(Effect::Deny, request) {
            return rule;
        }
        if let Some(rule) = project
            .iter()
            .find(|r| crate::rule::matches(r, &request.tool, request.command.as_deref()))
        {
            return Decision {
                effect: Effect::Allow,
                source: DecisionSource::Project(rule.id.to_string()),
                message: None,
            };
        }
        self.first(Effect::Ask, request)
            .or_else(|| self.first(Effect::Allow, request))
            .unwrap_or(Decision {
                effect: Effect::Ask,
                source: DecisionSource::Default,
                message: None,
            })
    }

    fn first(&self, effect: Effect, request: &ToolRequest) -> Option<Decision> {
        self.rules
            .iter()
            .find(|r| r.effect == effect && r.matches(request))
            .map(|rule| Decision {
                effect,
                source: DecisionSource::Rule(rule.id.clone()),
                message: None,
            })
    }
}

fn globs(
    rule: &str,
    patterns: &[String],
    literal_separator: bool,
) -> Result<Option<GlobSet>, PolicyError> {
    if patterns.is_empty() {
        return Ok(None);
    }
    let mut set = GlobSetBuilder::new();
    for pattern in patterns {
        let glob: Glob = GlobBuilder::new(pattern)
            .literal_separator(literal_separator)
            .backslash_escape(true)
            .build()
            .map_err(|_| PolicyError::InvalidGlob {
                rule: rule.to_string(),
                glob: pattern.clone(),
            })?;
        set.add(glob);
    }
    set.build().map(Some).map_err(|_| PolicyError::InvalidGlob {
        rule: rule.to_string(),
        glob: patterns.join(", "),
    })
}

impl Rule {
    fn matches(&self, request: &ToolRequest) -> bool {
        if !self.tools.contains(&request.tool) {
            return false;
        }
        if self.effect == Effect::Allow && request.command.as_deref().is_some_and(has_metacharacter)
        {
            return false;
        }
        if let Some(paths) = &self.paths {
            match &request.path {
                Some(p) if p.inside_worktree && paths.is_match(&p.path) => {}
                _ => return false,
            }
        }
        if let Some(commands) = &self.commands {
            match request.command.as_deref() {
                Some(c) if commands.is_match(c.trim()) => {}
                _ => return false,
            }
        }
        true
    }
}

pub const ARCHITECT_READ_TOOLS: &[&str] = &["Read", "Glob", "Grep", "LS", "TodoWrite"];
pub const ARCHITECT_WRITE_TOOLS: &[&str] = &["Write", "Edit", "MultiEdit"];
pub const ARCHITECT_READ: &str = "read";
pub const ARCHITECT_TARGET: &str = "target";
pub const ARCHITECT_DENY: &str = "deny";

#[must_use]
pub fn architect(request: &ToolRequest, target: &str, operator: &Policy) -> Decision {
    if let Some(name) = builtin_deny(request) {
        return Decision {
            effect: Effect::Deny,
            source: DecisionSource::Builtin(name.to_string()),
            message: None,
        };
    }
    if let Some(rule) = operator.first(Effect::Deny, request) {
        return rule;
    }
    let tool = request.tool.as_str();
    let allow = |name: &str| Decision {
        effect: Effect::Allow,
        source: DecisionSource::Architect(name.to_string()),
        message: None,
    };
    if ARCHITECT_READ_TOOLS.contains(&tool) {
        return allow(ARCHITECT_READ);
    }
    let writes_target = request
        .path
        .as_ref()
        .is_some_and(|p| p.inside_worktree && !p.through_symlink && p.path == target);
    if ARCHITECT_WRITE_TOOLS.contains(&tool) && writes_target {
        return allow(ARCHITECT_TARGET);
    }
    Decision {
        effect: Effect::Deny,
        source: DecisionSource::Architect(ARCHITECT_DENY.to_string()),
        message: Some(format!(
            "Architect sessions can read the repository and write only {target}."
        )),
    }
}

pub fn has_metacharacter(command: &str) -> bool {
    METACHARACTERS.iter().any(|m| command.contains(m))
}

fn builtin_deny(request: &ToolRequest) -> Option<&'static str> {
    if request.path.as_ref().is_some_and(|p| !p.inside_worktree) {
        return Some(BUILTIN_PATH_OUTSIDE_WORKTREE);
    }
    if request.command.as_deref().is_some_and(runs_git_push) {
        return Some(BUILTIN_GIT_PUSH);
    }
    None
}

fn runs_git_push(command: &str) -> bool {
    command
        .split([';', '&', '|', '\n', '\r', '(', ')', '`'])
        .any(|part| {
            let mut words = part
                .split_whitespace()
                .map(|w| w.trim_matches(|c| c == '"' || c == '\''));
            if !words.by_ref().any(|w| w == "git" || w.ends_with("/git")) {
                return false;
            }
            // Skip global options (and the values of those that take one) to find the subcommand.
            while let Some(word) = words.next() {
                if matches!(
                    word,
                    "-C" | "-c" | "--git-dir" | "--work-tree" | "--namespace"
                ) {
                    words.next();
                } else if !word.starts_with('-') {
                    return word == "push";
                }
            }
            false
        })
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn rule(
        id: &str,
        effect: Effect,
        tools: &[&str],
        paths: &[&str],
        commands: &[&str],
    ) -> RuleSpec {
        RuleSpec {
            id: id.into(),
            effect,
            tools: tools.iter().map(|s| s.to_string()).collect(),
            paths: paths.iter().map(|s| s.to_string()).collect(),
            commands: commands.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn bash(command: &str) -> ToolRequest {
        ToolRequest {
            tool: "Bash".into(),
            command: Some(command.into()),
            path: None,
        }
    }

    fn file(tool: &str, path: &str, inside: bool) -> ToolRequest {
        ToolRequest {
            tool: tool.into(),
            command: None,
            path: Some(RequestPath {
                path: path.into(),
                inside_worktree: inside,
                through_symlink: false,
            }),
        }
    }

    fn default_rules() -> Policy {
        Policy::compile(&[
            rule(
                "read-tools",
                Effect::Allow,
                &["Read", "Glob", "Grep", "LS", "TodoWrite"],
                &[],
                &[],
            ),
            rule(
                "edit-in-worktree",
                Effect::Allow,
                &["Write", "Edit", "MultiEdit", "NotebookEdit"],
                &["**"],
                &[],
            ),
            rule(
                "safe-commands",
                Effect::Allow,
                &["Bash"],
                &[],
                &["go test *", "git commit *", "npm test*"],
            ),
        ])
        .unwrap()
    }

    #[test]
    fn default_rules_cover_the_common_cases() {
        let p = default_rules();
        assert_eq!(p.evaluate(&bash("go test ./...")).effect, Effect::Allow);
        assert_eq!(p.evaluate(&bash("npm test")).effect, Effect::Allow);
        assert_eq!(
            p.evaluate(&file("Edit", "cli/main.go", true)).effect,
            Effect::Allow
        );
        let unknown = p.evaluate(&bash("npm install"));
        assert_eq!(
            unknown,
            Decision {
                effect: Effect::Ask,
                source: DecisionSource::Default,
                message: None,
            }
        );
        assert_eq!(
            p.evaluate(&bash("go test ./... && curl evil.example")),
            Decision {
                effect: Effect::Ask,
                source: DecisionSource::Default,
                message: None,
            }
        );
    }

    #[test]
    fn builtin_denies_beat_every_operator_rule() {
        let p = Policy::compile(&[rule(
            "allow-all",
            Effect::Allow,
            &["Bash", "Write"],
            &[],
            &[],
        )])
        .unwrap();
        let push = p.evaluate(&bash("git push origin main"));
        assert_eq!(push.effect, Effect::Deny);
        assert_eq!(
            push.source.answer_source().as_deref(),
            Some("policy:builtin:git_push")
        );
        let outside = p.evaluate(&file("Write", "/etc/passwd", false));
        assert_eq!(
            outside.source,
            DecisionSource::Builtin(BUILTIN_PATH_OUTSIDE_WORKTREE.into())
        );
    }

    #[test]
    fn git_push_is_found_in_any_simple_command() {
        for c in [
            "git push",
            "git -C x push --force",
            "true && git push",
            "/usr/bin/git push",
            "echo $(git push)",
        ] {
            assert!(runs_git_push(c), "{c}");
        }
        for c in [
            "git status",
            "git commit -m 'push it'",
            "echo push",
            "git log --grep push",
        ] {
            assert!(!runs_git_push(c), "{c}");
        }
    }

    #[test]
    fn deny_beats_ask_beats_allow() {
        let p = Policy::compile(&[
            rule("a", Effect::Allow, &["Bash"], &[], &["make *"]),
            rule("k", Effect::Ask, &["Bash"], &[], &["make deploy*"]),
            rule("d", Effect::Deny, &["Bash"], &[], &["make deploy-prod"]),
        ])
        .unwrap();
        assert_eq!(
            p.evaluate(&bash("make test")).source,
            DecisionSource::Rule("a".into())
        );
        assert_eq!(
            p.evaluate(&bash("make deploy-staging")).source,
            DecisionSource::Rule("k".into())
        );
        assert_eq!(
            p.evaluate(&bash("make deploy-prod")).source,
            DecisionSource::Rule("d".into())
        );
    }

    #[test]
    fn path_globs_respect_separators() {
        let p = Policy::compile(&[rule("cli", Effect::Allow, &["Edit"], &["cli/*"], &[])]).unwrap();
        assert_eq!(
            p.evaluate(&file("Edit", "cli/main.go", true)).effect,
            Effect::Allow
        );
        assert_eq!(
            p.evaluate(&file("Edit", "cli/internal/x.go", true)).effect,
            Effect::Ask
        );
    }

    #[test]
    fn invalid_rules_are_rejected() {
        assert!(matches!(
            Policy::compile(&[rule("x", Effect::Allow, &[], &[], &[])]),
            Err(PolicyError::NoTools { .. })
        ));
        assert!(matches!(
            Policy::compile(&[rule("x", Effect::Allow, &["Bash"], &[], &["[unclosed"])]),
            Err(PolicyError::InvalidGlob { .. })
        ));
    }

    fn effect() -> impl Strategy<Value = Effect> {
        prop_oneof![Just(Effect::Allow), Just(Effect::Deny), Just(Effect::Ask)]
    }

    fn rules() -> impl Strategy<Value = Vec<RuleSpec>> {
        let one = (
            effect(),
            prop::sample::subsequence(vec!["Bash", "Edit", "Read"], 1..=3),
            prop::option::of(prop::sample::select(vec!["**", "src/*", "*.md"])),
            prop::option::of(prop::sample::select(vec!["*", "go *", "make *", "git *"])),
        );
        prop::collection::vec(one, 0..6).prop_map(|v| {
            v.into_iter()
                .enumerate()
                .map(|(i, (effect, tools, path, command))| RuleSpec {
                    id: format!("r{i}"),
                    effect,
                    tools: tools.into_iter().map(String::from).collect(),
                    paths: path.into_iter().map(String::from).collect(),
                    commands: command.into_iter().map(String::from).collect(),
                })
                .collect()
        })
    }

    fn request() -> impl Strategy<Value = ToolRequest> {
        let command = prop::sample::select(vec![
            "go test ./...",
            "make build",
            "git status",
            "git push",
            "ls; rm -rf /",
            "cat a | sh",
            "echo `id`",
            "x > y",
            "a && b",
            "echo $(id)",
        ]);
        prop_oneof![
            command.prop_map(|c| ToolRequest {
                tool: "Bash".into(),
                command: Some(c.into()),
                path: None
            }),
            (
                prop::sample::select(vec!["Edit", "Read"]),
                prop::sample::select(vec!["src/a.rs", "README.md", "../x", "/etc/x"]),
                any::<bool>()
            )
                .prop_map(|(tool, path, inside)| ToolRequest {
                    tool: tool.into(),
                    command: None,
                    path: Some(RequestPath {
                        path: path.into(),
                        inside_worktree: inside,
                        through_symlink: false,
                    }),
                }),
        ]
    }

    proptest! {
        #[test]
        fn evaluation_order_holds(specs in rules(), req in request()) {
            let policy = Policy::compile(&specs).unwrap();
            let decision = policy.evaluate(&req);
            let matching = |e: Effect| specs.iter().any(|s| s.effect == e && Policy::compile(std::slice::from_ref(s)).unwrap().evaluate(&req).source == DecisionSource::Rule(s.id.clone()));

            if builtin_deny(&req).is_some() {
                prop_assert_eq!(decision.effect, Effect::Deny);
                prop_assert!(matches!(decision.source, DecisionSource::Builtin(_)));
            } else if matching(Effect::Deny) {
                prop_assert_eq!(decision.effect, Effect::Deny);
            } else if matching(Effect::Ask) {
                prop_assert_eq!(decision.effect, Effect::Ask);
            } else if matching(Effect::Allow) {
                prop_assert_eq!(decision.effect, Effect::Allow);
            } else {
                prop_assert_eq!(decision, Decision { effect: Effect::Ask, source: DecisionSource::Default, message: None });
            }
        }

        #[test]
        fn metacharacter_commands_are_never_allowed(specs in rules(), req in request()) {
            let decision = Policy::compile(&specs).unwrap().evaluate(&req);
            if req.command.as_deref().is_some_and(has_metacharacter) {
                prop_assert_ne!(decision.effect, Effect::Allow);
            }
        }

        #[test]
        fn paths_outside_the_worktree_are_always_denied(specs in rules(), path in "[a-z./]{1,12}") {
            let req = ToolRequest { tool: "Edit".into(), command: None, path: Some(RequestPath { path, inside_worktree: false, through_symlink: false }) };
            prop_assert_eq!(Policy::compile(&specs).unwrap().evaluate(&req).effect, Effect::Deny);
        }
    }
}

#[cfg(test)]
mod architect_tests {
    use proptest::prelude::*;

    use super::*;

    const TARGET: &str = "docs/specs/avatar-upload.md";

    fn at(tool: &str, path: Option<(&str, bool, bool)>, command: Option<&str>) -> ToolRequest {
        ToolRequest {
            tool: tool.into(),
            command: command.map(String::from),
            path: path.map(|(path, inside, symlink)| RequestPath {
                path: path.into(),
                inside_worktree: inside,
                through_symlink: symlink,
            }),
        }
    }

    fn operator() -> Policy {
        Policy::compile(&[
            RuleSpec {
                id: "allow-everything".into(),
                effect: Effect::Allow,
                tools: ["Bash", "Write", "Edit", "WebFetch", "Task", "Read"]
                    .map(String::from)
                    .to_vec(),
                paths: vec![],
                commands: vec![],
            },
            RuleSpec {
                id: "no-secrets".into(),
                effect: Effect::Deny,
                tools: vec!["Read".into()],
                paths: vec!["**/.env".into()],
                commands: vec![],
            },
        ])
        .unwrap()
    }

    fn source(d: &Decision) -> Option<String> {
        d.source.answer_source()
    }

    #[test]
    fn read_tools_inside_the_worktree_are_allowed() {
        for tool in ARCHITECT_READ_TOOLS {
            let d = architect(
                &at(tool, Some(("docs/prd.md", true, false)), None),
                TARGET,
                &operator(),
            );
            assert_eq!(d.effect, Effect::Allow, "{tool}");
            assert_eq!(source(&d).as_deref(), Some("policy:architect:read"));
        }
        let todo = architect(&at("TodoWrite", None, None), TARGET, &operator());
        assert_eq!(source(&todo).as_deref(), Some("policy:architect:read"));
    }

    #[test]
    fn write_tools_are_allowed_only_for_the_exact_target() {
        for tool in ARCHITECT_WRITE_TOOLS {
            let d = architect(
                &at(tool, Some((TARGET, true, false)), None),
                TARGET,
                &operator(),
            );
            assert_eq!(d.effect, Effect::Allow, "{tool}");
            assert_eq!(source(&d).as_deref(), Some("policy:architect:target"));
        }
        let near_misses = [
            "docs/specs/Avatar-upload.md",
            "docs/specs/avatar-upload.MD",
            "docs/specs/./avatar-upload.md",
            "docs/specs/../specs/avatar-upload.md",
            "./docs/specs/avatar-upload.md",
            "docs/specs/avatar-upload.md/",
            "docs/specs/other.md",
            "docs/prd.md",
            "specs/avatar-upload.md",
        ];
        for path in near_misses {
            let d = architect(
                &at("Write", Some((path, true, false)), None),
                TARGET,
                &operator(),
            );
            assert_eq!(d.effect, Effect::Deny, "{path}");
            assert_eq!(source(&d).as_deref(), Some("policy:architect:deny"));
            assert_eq!(
                d.message.as_deref(),
                Some(
                    "Architect sessions can read the repository and write only docs/specs/avatar-upload.md."
                )
            );
        }
    }

    #[test]
    fn a_target_reached_through_a_symlink_is_denied() {
        let d = architect(
            &at("Edit", Some((TARGET, true, true)), None),
            TARGET,
            &operator(),
        );
        assert_eq!(d.effect, Effect::Deny);
        assert_eq!(source(&d).as_deref(), Some("policy:architect:deny"));
    }

    #[test]
    fn commands_web_tasks_notebooks_and_unknown_tools_are_denied() {
        for tool in [
            "Bash",
            "WebFetch",
            "WebSearch",
            "Task",
            "NotebookEdit",
            "mcp__x__y",
            "Frobnicate",
        ] {
            let d = architect(&at(tool, None, Some("ls")), TARGET, &operator());
            assert_eq!(d.effect, Effect::Deny, "{tool}");
            assert_eq!(source(&d).as_deref(), Some("policy:architect:deny"));
        }
        let notebook = architect(
            &at("NotebookEdit", Some((TARGET, true, false)), None),
            TARGET,
            &operator(),
        );
        assert_eq!(notebook.effect, Effect::Deny);
    }

    #[test]
    fn builtin_and_operator_denies_come_first() {
        let outside = architect(
            &at("Read", Some(("/etc/passwd", false, false)), None),
            TARGET,
            &operator(),
        );
        assert_eq!(
            source(&outside).as_deref(),
            Some("policy:builtin:path_outside_worktree")
        );
        let push = architect(
            &at("Bash", None, Some("git push origin main")),
            TARGET,
            &operator(),
        );
        assert_eq!(source(&push).as_deref(), Some("policy:builtin:git_push"));
        let env = architect(
            &at("Read", Some(("app/.env", true, false)), None),
            TARGET,
            &operator(),
        );
        assert_eq!(source(&env).as_deref(), Some("policy:no-secrets"));
        let target_outside = architect(
            &at("Write", Some((TARGET, false, false)), None),
            TARGET,
            &operator(),
        );
        assert_eq!(
            source(&target_outside).as_deref(),
            Some("policy:builtin:path_outside_worktree")
        );
    }

    fn request() -> impl Strategy<Value = ToolRequest> {
        let tools = prop::sample::select(vec![
            "Read",
            "Glob",
            "Grep",
            "LS",
            "TodoWrite",
            "Write",
            "Edit",
            "MultiEdit",
            "Bash",
            "WebFetch",
            "WebSearch",
            "Task",
            "NotebookEdit",
            "Other",
        ]);
        let paths = prop_oneof![
            Just(TARGET.to_string()),
            Just("docs/specs/A.md".to_string()),
            Just("docs/specs/a.md".to_string()),
            "[a-zA-Z./]{1,24}",
        ];
        (
            tools,
            prop::option::of((paths, any::<bool>(), any::<bool>())),
            prop::option::of(prop::sample::select(vec![
                "ls", "git push", "rm -rf /", "cat x",
            ])),
        )
            .prop_map(|(tool, path, command)| ToolRequest {
                tool: tool.into(),
                command: command.map(String::from),
                path: path.map(|(path, inside, symlink)| RequestPath {
                    path,
                    inside_worktree: inside,
                    through_symlink: symlink,
                }),
            })
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(512))]

        #[test]
        fn the_architect_never_asks_and_writes_only_the_target(req in request(), target in prop::sample::select(vec![TARGET, "docs/specs/a.md", "specs/x.md"])) {
            let d = architect(&req, target, &operator());
            prop_assert_ne!(d.effect, Effect::Ask);
            prop_assert!(d.source.answer_source().is_some());
            if d.effect == Effect::Allow {
                let tool = req.tool.as_str();
                let read = ARCHITECT_READ_TOOLS.contains(&tool);
                let exact = req.path.as_ref().is_some_and(|p| p.path == target && p.inside_worktree && !p.through_symlink);
                prop_assert!(read || (ARCHITECT_WRITE_TOOLS.contains(&tool) && exact));
                prop_assert!(req.path.as_ref().is_none_or(|p| p.inside_worktree));
                prop_assert!(!matches!(tool, "Bash" | "WebFetch" | "WebSearch" | "Task" | "NotebookEdit"));
            }
            if ARCHITECT_WRITE_TOOLS.contains(&req.tool.as_str()) && d.effect == Effect::Allow {
                prop_assert_eq!(d.source.answer_source(), Some("policy:architect:target".to_string()));
            }
        }

        #[test]
        fn operator_allow_rules_and_project_rules_never_widen_the_architect(req in request()) {
            let d = architect(&req, TARGET, &operator());
            let bare = architect(&req, TARGET, &Policy::compile(&[]).unwrap());
            if d.effect == Effect::Allow {
                prop_assert_eq!(bare.effect, Effect::Allow);
            }
        }
    }
}
