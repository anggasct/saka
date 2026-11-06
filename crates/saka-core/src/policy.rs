use globset::{Glob, GlobBuilder, GlobSet, GlobSetBuilder};
use serde::{Deserialize, Serialize};

use crate::model::PermissionRule;
use crate::shell;

pub const BUILTIN_PATH_OUTSIDE_WORKTREE: &str = "path_outside_worktree";
pub const BUILTIN_GIT_PUSH: &str = "git_push";

pub use crate::vocab::Effect;

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
    Reviewer(String),
    Default,
}

impl DecisionSource {
    pub fn answer_source(&self) -> Option<String> {
        match self {
            DecisionSource::Builtin(name) => Some(format!("policy:builtin:{name}")),
            DecisionSource::Rule(id) => Some(format!("policy:{id}")),
            DecisionSource::Project(id) => Some(format!("policy:project:{id}")),
            DecisionSource::Architect(name) => Some(format!("policy:architect:{name}")),
            DecisionSource::Reviewer(name) => Some(format!("policy:reviewer:{name}")),
            DecisionSource::Default => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartDecision {
    pub command: String,
    pub effect: Effect,
    pub source: DecisionSource,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub effect: Effect,
    pub source: DecisionSource,
    pub message: Option<String>,
    pub parts: Vec<PartDecision>,
}

impl Decision {
    pub fn new(effect: Effect, source: DecisionSource) -> Decision {
        Decision {
            effect,
            source,
            message: None,
            parts: Vec::new(),
        }
    }
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
    specs: Vec<RuleSpec>,
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
        Ok(Policy {
            specs: specs.to_vec(),
            rules,
        })
    }

    pub fn specs(&self) -> &[RuleSpec] {
        &self.specs
    }

    pub fn evaluate(&self, request: &ToolRequest) -> Decision {
        self.evaluate_with(request, &[])
    }

    pub fn evaluate_with(&self, request: &ToolRequest, project: &[PermissionRule]) -> Decision {
        if let Some(name) = builtin_deny(request) {
            return Decision::new(Effect::Deny, DecisionSource::Builtin(name.to_string()));
        }
        if let Some(rule) = self.first(Effect::Deny, request) {
            return rule;
        }
        if let Some(rule) = project_first(Effect::Deny, request, project, true) {
            return rule;
        }
        match (request.tool.as_str(), request.command.as_deref()) {
            ("Bash", Some(command)) => match shell::split(command) {
                Some(parts) => self.evaluate_parts(&parts, project),
                None => self.settle(request, project, false),
            },
            _ => self.settle(request, project, true),
        }
    }

    fn evaluate_parts(&self, parts: &[String], project: &[PermissionRule]) -> Decision {
        let mut decided: Vec<PartDecision> = Vec::new();
        for part in parts {
            if decided.iter().any(|d| &d.command == part) {
                continue;
            }
            let request = ToolRequest {
                tool: "Bash".to_string(),
                command: Some(part.clone()),
                path: None,
            };
            let decision = self.settle(&request, project, true);
            decided.push(PartDecision {
                command: part.clone(),
                effect: decision.effect,
                source: decision.source,
            });
        }
        let deciding = [Effect::Deny, Effect::Ask]
            .iter()
            .find_map(|effect| decided.iter().find(|d| d.effect == *effect))
            .or(decided.first());
        match deciding {
            Some(d) => Decision {
                effect: d.effect,
                source: d.source.clone(),
                message: None,
                parts: decided,
            },
            None => Decision::new(Effect::Ask, DecisionSource::Default),
        }
    }

    fn settle(
        &self,
        request: &ToolRequest,
        project: &[PermissionRule],
        glob_allow: bool,
    ) -> Decision {
        if let Some(rule) = self.first(Effect::Deny, request) {
            return rule;
        }
        project_first(Effect::Deny, request, project, glob_allow)
            .or_else(|| project_first(Effect::Ask, request, project, glob_allow))
            .or_else(|| project_first(Effect::Allow, request, project, glob_allow))
            .or_else(|| self.first(Effect::Ask, request))
            .or_else(|| {
                glob_allow
                    .then(|| self.first(Effect::Allow, request))
                    .flatten()
            })
            .unwrap_or(Decision::new(Effect::Ask, DecisionSource::Default))
    }

    fn first(&self, effect: Effect, request: &ToolRequest) -> Option<Decision> {
        self.rules
            .iter()
            .find(|r| r.effect == effect && r.matches(request))
            .map(|rule| Decision::new(effect, DecisionSource::Rule(rule.id.clone())))
    }
}

fn project_first(
    effect: Effect,
    request: &ToolRequest,
    project: &[PermissionRule],
    glob_allow: bool,
) -> Option<Decision> {
    project
        .iter()
        .find(|r| r.effect == effect && crate::rule::matches(r, request, glob_allow))
        .map(|r| Decision::new(effect, DecisionSource::Project(r.id.to_string())))
}

fn glob_builder(pattern: &str, literal_separator: bool) -> GlobBuilder<'_> {
    let mut builder = GlobBuilder::new(pattern);
    builder
        .literal_separator(literal_separator)
        .backslash_escape(true);
    builder
}

pub(crate) fn glob_compiles(pattern: &str, literal_separator: bool) -> bool {
    glob_builder(pattern, literal_separator).build().is_ok()
}

pub(crate) fn glob_matches(pattern: &str, literal_separator: bool, text: &str) -> bool {
    glob_builder(pattern, literal_separator)
        .build()
        .is_ok_and(|g| g.compile_matcher().is_match(text))
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
        let glob: Glob = glob_builder(pattern, literal_separator)
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
        return Decision::new(Effect::Deny, DecisionSource::Builtin(name.to_string()));
    }
    if let Some(rule) = operator.first(Effect::Deny, request) {
        return rule;
    }
    let tool = request.tool.as_str();
    let allow =
        |name: &str| Decision::new(Effect::Allow, DecisionSource::Architect(name.to_string()));
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
        message: Some(format!(
            "Architect sessions can read the repository and write only {target}."
        )),
        ..Decision::new(
            Effect::Deny,
            DecisionSource::Architect(ARCHITECT_DENY.to_string()),
        )
    }
}

pub const REVIEWER_READ: &str = "read";
pub const REVIEWER_DENY: &str = "deny";

#[must_use]
pub fn reviewer(request: &ToolRequest, operator: &Policy) -> Decision {
    if let Some(name) = builtin_deny(request) {
        return Decision::new(Effect::Deny, DecisionSource::Builtin(name.to_string()));
    }
    if let Some(rule) = operator.first(Effect::Deny, request) {
        return rule;
    }
    let reads = ARCHITECT_READ_TOOLS.contains(&request.tool.as_str());
    let inside = request.path.as_ref().is_none_or(|p| p.inside_worktree);
    let (effect, name, message) = if reads && inside {
        (Effect::Allow, REVIEWER_READ, None)
    } else {
        (
            Effect::Deny,
            REVIEWER_DENY,
            Some("Reviewer sessions can only read files in the checkout.".to_string()),
        )
    };
    Decision {
        message,
        ..Decision::new(effect, DecisionSource::Reviewer(name.to_string()))
    }
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
        assert_eq!(unknown.effect, Effect::Ask);
        assert_eq!(unknown.source, DecisionSource::Default);
        let mixed = p.evaluate(&bash("go test ./... && curl evil.example"));
        assert_eq!(mixed.effect, Effect::Ask);
        assert_eq!(mixed.source, DecisionSource::Default);
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

    fn allow_bash(id: &str, commands: &[&str]) -> RuleSpec {
        rule(id, Effect::Allow, &["Bash"], &[], commands)
    }

    fn project_rule_of(
        id: u128,
        effect: Effect,
        match_kind: crate::vocab::MatchKind,
        tool: &str,
        command: Option<&str>,
        path: Option<&str>,
    ) -> PermissionRule {
        use crate::ids::{PermissionRuleId, ProjectId};
        use crate::time::Timestamp;
        PermissionRule {
            id: PermissionRuleId(uuid::Uuid::from_u128(id)),
            project_id: ProjectId(uuid::Uuid::from_u128(1)),
            effect,
            tool: tool.into(),
            match_kind,
            command: command.map(String::from),
            path: path.map(String::from),
            note: None,
            permission_request_id: None,
            created_by: "human:operator:local".into(),
            created_at: Timestamp(0),
            revoked_at: None,
            revoke_reason: None,
        }
    }

    fn project_rule(id: u128, command: &str) -> PermissionRule {
        project_rule_of(
            id,
            Effect::Allow,
            crate::vocab::MatchKind::Exact,
            "Bash",
            Some(command),
            None,
        )
    }

    fn project_glob(id: u128, effect: Effect, command: &str) -> PermissionRule {
        project_rule_of(
            id,
            effect,
            crate::vocab::MatchKind::Glob,
            "Bash",
            Some(command),
            None,
        )
    }

    #[test]
    fn every_pair_of_colliding_levels_resolves_in_the_documented_order() {
        let operator = |effect| rule("op", effect, &["Bash"], &[], &["make *"]);
        let levels: Vec<(&str, Option<RuleSpec>, Option<PermissionRule>)> = vec![
            ("policy:op", Some(operator(Effect::Deny)), None),
            (
                "policy:project:00000000-0000-0000-0000-000000000011",
                None,
                Some(project_glob(0x11, Effect::Deny, "make *")),
            ),
            (
                "policy:project:00000000-0000-0000-0000-000000000012",
                None,
                Some(project_glob(0x12, Effect::Ask, "make *")),
            ),
            (
                "policy:project:00000000-0000-0000-0000-000000000013",
                None,
                Some(project_glob(0x13, Effect::Allow, "make *")),
            ),
            ("policy:op", Some(operator(Effect::Ask)), None),
            ("policy:op", Some(operator(Effect::Allow)), None),
        ];
        let effects = [
            Effect::Deny,
            Effect::Deny,
            Effect::Ask,
            Effect::Allow,
            Effect::Ask,
            Effect::Allow,
        ];
        for i in 0..levels.len() {
            for j in (i + 1)..levels.len() {
                let specs: Vec<RuleSpec> = [&levels[i], &levels[j]]
                    .iter()
                    .filter_map(|l| l.1.clone())
                    .collect();
                let project: Vec<PermissionRule> = [&levels[i], &levels[j]]
                    .iter()
                    .filter_map(|l| l.2.clone())
                    .collect();
                let d = Policy::compile(&specs)
                    .unwrap()
                    .evaluate_with(&bash("make test"), &project);
                assert_eq!(d.effect, effects[i], "level {i} against {j}");
                assert_eq!(
                    d.source.answer_source().as_deref(),
                    Some(levels[i].0),
                    "level {i} against {j}"
                );
            }
        }
        let default = Policy::compile(&[]).unwrap().evaluate(&bash("make test"));
        assert_eq!(
            (default.effect, default.source),
            (Effect::Ask, DecisionSource::Default)
        );
    }

    #[test]
    fn the_builtin_deny_beats_every_rule_of_every_level() {
        let operator = Policy::compile(&[allow_bash("any", &["git *"])]).unwrap();
        let project = [
            project_glob(0x21, Effect::Allow, "git *"),
            project_rule(0x22, "git push"),
        ];
        let d = operator.evaluate_with(&bash("git push"), &project);
        assert_eq!(d.source, DecisionSource::Builtin(BUILTIN_GIT_PUSH.into()));
        let deny_first =
            Policy::compile(&[rule("no", Effect::Deny, &["Bash"], &[], &["git *"])]).unwrap();
        let d = deny_first.evaluate_with(&bash("git push"), &project);
        assert_eq!(d.source, DecisionSource::Builtin(BUILTIN_GIT_PUSH.into()));
    }

    #[test]
    fn a_project_allow_never_beats_an_operator_deny() {
        let p = Policy::compile(&[rule("no", Effect::Deny, &["Bash"], &[], &["rm *"])]).unwrap();
        let project = [
            project_rule(0x31, "rm x"),
            project_glob(0x32, Effect::Allow, "rm *"),
        ];
        let d = p.evaluate_with(&bash("ls && rm x"), &project);
        assert_eq!(
            (d.effect, d.source),
            (Effect::Deny, DecisionSource::Rule("no".into()))
        );
    }

    #[test]
    fn a_project_deny_or_ask_applies_to_a_command_that_cannot_be_split() {
        let p = Policy::compile(&[]).unwrap();
        let deny = [project_glob(0x41, Effect::Deny, "*rm -rf*")];
        assert_eq!(
            p.evaluate_with(&bash("echo $(rm -rf /)"), &deny).effect,
            Effect::Deny
        );
        let ask = [
            project_glob(0x42, Effect::Ask, "ls*"),
            project_rule(0x43, "ls $(pwd)"),
        ];
        let d = p.evaluate_with(&bash("ls $(pwd)"), &ask);
        assert_eq!(
            (d.effect, d.source),
            (Effect::Ask, DecisionSource::Project(ask[0].id.to_string()))
        );
    }

    #[test]
    fn a_project_allow_glob_never_applies_to_a_command_that_cannot_be_split() {
        let p = Policy::compile(&[]).unwrap();
        let project = [project_glob(0x51, Effect::Allow, "ls*")];
        assert_eq!(
            p.evaluate_with(&bash("ls -la"), &project).effect,
            Effect::Allow
        );
        assert_eq!(
            p.evaluate_with(&bash("ls $(pwd)"), &project).effect,
            Effect::Ask
        );
        assert_eq!(
            p.evaluate_with(&bash("ls && rm x"), &project).effect,
            Effect::Ask
        );
    }

    #[test]
    fn project_rules_on_files_match_by_exact_path_or_glob_inside_the_worktree() {
        use crate::vocab::MatchKind::{Exact, Glob};
        let p = Policy::compile(&[]).unwrap();
        let exact = [project_rule_of(
            0x61,
            Effect::Allow,
            Exact,
            "Edit",
            None,
            Some("src/a.rs"),
        )];
        assert_eq!(
            p.evaluate_with(&file("Edit", "src/a.rs", true), &exact)
                .effect,
            Effect::Allow
        );
        assert_eq!(
            p.evaluate_with(&file("Edit", "src/b.rs", true), &exact)
                .effect,
            Effect::Ask
        );
        let glob = [project_rule_of(
            0x62,
            Effect::Allow,
            Glob,
            "Edit",
            None,
            Some("src/*"),
        )];
        assert_eq!(
            p.evaluate_with(&file("Edit", "src/b.rs", true), &glob)
                .effect,
            Effect::Allow
        );
        assert_eq!(
            p.evaluate_with(&file("Edit", "src/x/b.rs", true), &glob)
                .effect,
            Effect::Ask
        );
        let any_path = [project_rule_of(
            0x63,
            Effect::Allow,
            Exact,
            "Edit",
            None,
            None,
        )];
        assert_eq!(
            p.evaluate_with(&file("Edit", "a.md", true), &any_path)
                .effect,
            Effect::Allow
        );
        assert_eq!(
            p.evaluate_with(&file("Edit", "/etc/x", false), &any_path)
                .effect,
            Effect::Deny
        );
        let mut through = file("Edit", "src/a.rs", true);
        if let Some(path) = through.path.as_mut() {
            path.through_symlink = true;
        }
        assert_eq!(p.evaluate_with(&through, &exact).effect, Effect::Ask);
        let deny = [project_rule_of(
            0x64,
            Effect::Deny,
            Glob,
            "Edit",
            None,
            Some("**/.env"),
        )];
        assert_eq!(
            p.evaluate_with(&file("Edit", "app/.env", true), &deny)
                .effect,
            Effect::Deny
        );
    }

    #[test]
    fn a_revoked_project_rule_does_not_apply() {
        let p = Policy::compile(&[]).unwrap();
        let mut gone = project_rule(0x71, "ls");
        gone.revoked_at = Some(crate::time::Timestamp(5));
        assert_eq!(p.evaluate_with(&bash("ls"), &[gone]).effect, Effect::Ask);
    }

    #[test]
    fn a_compound_command_is_allowed_only_when_every_part_is_allowed_by_a_rule() {
        let p = Policy::compile(&[allow_bash("safe", &["git status", "ls*"])]).unwrap();
        let both = p.evaluate(&bash("git status && ls -la"));
        assert_eq!(both.effect, Effect::Allow);
        assert_eq!(both.source, DecisionSource::Rule("safe".into()));
        assert_eq!(
            both.parts
                .iter()
                .map(|d| d.command.as_str())
                .collect::<Vec<_>>(),
            vec!["git status", "ls -la"]
        );
        let one_unknown = p.evaluate(&bash("ls && rm -rf x"));
        assert_eq!(one_unknown.effect, Effect::Ask);
        assert_eq!(one_unknown.source, DecisionSource::Default);
        assert_eq!(one_unknown.parts[0].effect, Effect::Allow);
        assert_eq!(one_unknown.parts[1].effect, Effect::Ask);
        assert_eq!(p.evaluate(&bash("ls | grep x")).effect, Effect::Ask);
    }

    #[test]
    fn a_part_that_cannot_be_vouched_for_is_never_allowed_by_a_glob() {
        let p = Policy::compile(&[allow_bash("safe", &["ls*", "git *"])]).unwrap();
        for command in [
            "ls && $(cat x)",
            "ls `id`",
            "ls > out",
            "ls & rm x",
            "ls; sudo x",
            "FOO=1 ls",
            "ls \\\n -la",
        ] {
            let d = p.evaluate(&bash(command));
            assert_eq!(d.effect, Effect::Ask, "{command}");
            assert!(d.parts.is_empty(), "{command}");
        }
    }

    #[test]
    fn the_builtin_push_deny_sees_the_whole_compound_command() {
        let p = Policy::compile(&[allow_bash("any", &["*"])]).unwrap();
        for command in [
            "ls; git push origin main",
            "ls && git push",
            "ls | git push",
        ] {
            let d = p.evaluate(&bash(command));
            assert_eq!(d.effect, Effect::Deny, "{command}");
            assert_eq!(d.source, DecisionSource::Builtin(BUILTIN_GIT_PUSH.into()));
        }
    }

    #[test]
    fn a_deny_or_ask_on_one_part_decides_the_whole_command() {
        let p = Policy::compile(&[
            allow_bash("a", &["make *", "ls"]),
            rule("k", Effect::Ask, &["Bash"], &[], &["make deploy*"]),
            rule("d", Effect::Deny, &["Bash"], &[], &["rm *"]),
        ])
        .unwrap();
        let ask = p.evaluate(&bash("ls && make deploy-staging && make test"));
        assert_eq!(
            (ask.effect, ask.source),
            (Effect::Ask, DecisionSource::Rule("k".into()))
        );
        let deny = p.evaluate(&bash("make deploy-staging; ls && rm x"));
        assert_eq!(
            (deny.effect, deny.source),
            (Effect::Deny, DecisionSource::Rule("d".into()))
        );
    }

    #[test]
    fn a_glob_matches_one_part_and_never_crosses_into_the_next() {
        let p = Policy::compile(&[allow_bash("t", &["go test *"])]).unwrap();
        assert_eq!(p.evaluate(&bash("go test ./...")).effect, Effect::Allow);
        assert_eq!(
            p.evaluate(&bash("go test ./... ; curl x")).effect,
            Effect::Ask
        );
        assert_eq!(
            p.evaluate(&bash("go test ./... | tee out")).effect,
            Effect::Ask
        );
        let quoted = Policy::compile(&[allow_bash("e", &["echo *"])]).unwrap();
        assert_eq!(quoted.evaluate(&bash("echo \"a;b\"")).effect, Effect::Allow);
        assert_eq!(quoted.evaluate(&bash("echo a; rm x")).effect, Effect::Ask);
    }

    #[test]
    fn repeated_parts_are_evaluated_once_and_keep_their_order() {
        let p = Policy::compile(&[allow_bash("a", &["a", "b"])]).unwrap();
        let d = p.evaluate(&bash("a && b && a"));
        assert_eq!(
            d.parts
                .iter()
                .map(|d| d.command.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "b"]
        );
        assert_eq!(d.effect, Effect::Allow);
    }

    #[test]
    fn an_exact_project_rule_per_part_allows_the_compound_command() {
        let p = Policy::compile(&[]).unwrap();
        let rules = [project_rule(10, "git status"), project_rule(11, "ls")];
        let d = p.evaluate_with(&bash("git status && ls"), &rules);
        assert_eq!(d.effect, Effect::Allow);
        assert_eq!(d.source, DecisionSource::Project(rules[0].id.to_string()));
        assert_eq!(
            p.evaluate_with(&bash("git status && ls && rm x"), &rules)
                .effect,
            Effect::Ask
        );
        let unsplittable = p.evaluate_with(&bash("git status && $(ls)"), &rules);
        assert_eq!(unsplittable.effect, Effect::Ask);
        let whole = [project_rule(12, "git status && $(ls)")];
        assert_eq!(
            p.evaluate_with(&bash("git status && $(ls)"), &whole).effect,
            Effect::Allow,
            "an exact rule with the full text still applies when the command cannot be split"
        );
    }

    #[test]
    fn a_tool_that_is_not_bash_never_goes_through_the_splitter() {
        let p = Policy::compile(&[rule("w", Effect::Allow, &["Write"], &["**"], &[])]).unwrap();
        let d = p.evaluate(&file("Write", "a;b.txt", true));
        assert_eq!(d.effect, Effect::Allow);
        assert!(d.parts.is_empty());
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
            "go test ./... && make build",
            "git status && echo $(id)",
            "make build; git status",
            "echo \"a;b\"",
            "FOO=1 make build",
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
            } else if let Some(split) = req.command.as_deref().filter(|_| req.tool == "Bash").map(shell::split) {
                let expected = match split {
                    None => Effect::Ask,
                    Some(parts) => {
                        let effects: Vec<Effect> = parts.iter().map(|p| policy.evaluate(&bash(p)).effect).collect();
                        if effects.contains(&Effect::Deny) {
                            Effect::Deny
                        } else if effects.contains(&Effect::Ask) {
                            Effect::Ask
                        } else {
                            Effect::Allow
                        }
                    }
                };
                prop_assert_eq!(decision.effect, expected);
            } else if matching(Effect::Ask) {
                prop_assert_eq!(decision.effect, Effect::Ask);
            } else if matching(Effect::Allow) {
                prop_assert_eq!(decision.effect, Effect::Allow);
            } else {
                prop_assert_eq!(decision, Decision::new(Effect::Ask, DecisionSource::Default));
            }
        }

        #[test]
        fn a_command_that_cannot_be_split_is_never_allowed_by_a_glob(specs in rules(), req in request()) {
            let decision = Policy::compile(&specs).unwrap().evaluate(&req);
            if req.command.as_deref().is_some_and(|c| shell::split(c).is_none()) {
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

#[cfg(test)]
mod reviewer_tests {
    use proptest::prelude::*;

    use super::*;

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
    fn read_tools_inside_the_checkout_are_allowed() {
        for tool in ARCHITECT_READ_TOOLS {
            let d = reviewer(
                &at(tool, Some(("src/lib.rs", true, false)), None),
                &operator(),
            );
            assert_eq!(d.effect, Effect::Allow, "{tool}");
            assert_eq!(source(&d).as_deref(), Some("policy:reviewer:read"));
        }
        let bare = reviewer(&at("Grep", None, None), &operator());
        assert_eq!(bare.effect, Effect::Allow);
    }

    #[test]
    fn everything_that_changes_or_reaches_out_is_denied() {
        for tool in [
            "Bash",
            "Write",
            "Edit",
            "MultiEdit",
            "NotebookEdit",
            "WebFetch",
            "WebSearch",
            "Task",
            "Skill",
        ] {
            let d = reviewer(
                &at(tool, Some(("src/lib.rs", true, false)), Some("ls")),
                &operator(),
            );
            assert_eq!(d.effect, Effect::Deny, "{tool}");
            assert_eq!(source(&d).as_deref(), Some("policy:reviewer:deny"));
        }
    }

    #[test]
    fn paths_outside_the_checkout_are_denied_even_through_a_symlink() {
        for symlink in [false, true] {
            let d = reviewer(
                &at("Read", Some(("/etc/passwd", false, symlink)), None),
                &operator(),
            );
            assert_eq!(d.effect, Effect::Deny);
            assert_eq!(
                source(&d).as_deref(),
                Some("policy:builtin:path_outside_worktree")
            );
        }
    }

    #[test]
    fn an_operator_deny_rule_still_applies_to_reads() {
        let d = reviewer(&at("Read", Some((".env", true, false)), None), &operator());
        assert_eq!(d.effect, Effect::Deny);
        assert_eq!(source(&d).as_deref(), Some("policy:no-secrets"));
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
        (
            tools,
            prop::option::of(("[a-zA-Z./]{1,24}", any::<bool>(), any::<bool>())),
            prop::option::of(prop::sample::select(vec!["ls", "git push", "cat x"])),
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
        fn the_reviewer_never_asks_and_only_reads_inside_the_checkout(req in request()) {
            let d = reviewer(&req, &operator());
            prop_assert_ne!(d.effect, Effect::Ask);
            prop_assert!(d.source.answer_source().is_some());
            if d.effect == Effect::Allow {
                prop_assert!(ARCHITECT_READ_TOOLS.contains(&req.tool.as_str()));
                prop_assert!(req.path.as_ref().is_none_or(|p| p.inside_worktree));
            }
        }

        #[test]
        fn operator_allow_rules_never_widen_the_reviewer(req in request()) {
            let d = reviewer(&req, &operator());
            let bare = reviewer(&req, &Policy::compile(&[]).unwrap());
            if d.effect == Effect::Allow {
                prop_assert_eq!(bare.effect, Effect::Allow);
            }
        }
    }
}
