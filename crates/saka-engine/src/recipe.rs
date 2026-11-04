use std::collections::BTreeMap;

use saka_core::gate::GatePolicy;
use saka_core::vocab::{GateMode, SecretPurpose, Tool};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use toml::Spanned;

use crate::project_config::Issue;
use crate::versions::RepoFiles;

pub const RECIPE_PATH: &str = ".saka/environment.toml";
pub const MAX_COMMANDS: usize = 20;
pub const MAX_COMMAND_BYTES: usize = 4096;
pub const MAX_ENV_VARS: usize = 64;
pub const MAX_ENV_VALUE_BYTES: usize = 4096;
pub const MAX_SECRETS: usize = 64;

const RESERVED_NAMES: &[&str] = &[
    "PATH", "HOME", "TMPDIR", "LANG", "TERM", "USER", "SHELL", "PORT",
];
const RESERVED_PREFIXES: &[&str] = &["SAKA_", "GIT_", "CLAUDE_", "npm_config_"];
const RESERVED_GO: &[&str] = &[
    "GOPATH",
    "GOCACHE",
    "GOMODCACHE",
    "GOTOOLCHAIN",
    "GOFLAGS",
    "GOROOT",
];

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Recipe {
    pub ports: Option<u16>,
    pub env: BTreeMap<String, String>,
    pub setup: Vec<String>,
    pub teardown: Vec<String>,
    pub toolchain: BTreeMap<Tool, String>,
    pub secrets: BTreeMap<String, Vec<SecretPurpose>>,
    pub gate: Option<GatePolicy>,
}

const MAX_GATE_COMMANDS: usize = 20;
const MAX_REQUIRED_CHECKS: usize = 64;
const MAX_CHECK_NAME_BYTES: usize = 128;

impl Recipe {
    pub fn ports_or(&self, default: u16) -> u16 {
        self.ports.unwrap_or(default)
    }

    pub fn to_json(&self) -> Value {
        json!({
            "ports": self.ports,
            "env": self.env,
            "setup": self.setup,
            "teardown": self.teardown,
            "toolchain": self.toolchain,
            "secrets": self.secrets,
            "gate": self.gate.as_ref().map(|g| json!({
                "source": g.mode.as_str(),
                "commands": g.commands,
                "required_checks": g.required_checks,
                "timeout_minutes": g.timeout_minutes,
            })),
        })
    }

    pub fn from_json(value: &Value) -> Recipe {
        let strings = |key: &str| -> Vec<String> {
            value
                .get(key)
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(String::from)
                        .collect()
                })
                .unwrap_or_default()
        };
        Recipe {
            ports: value
                .get("ports")
                .and_then(Value::as_u64)
                .and_then(|p| u16::try_from(p).ok()),
            env: value
                .get("env")
                .and_then(Value::as_object)
                .map(|m| {
                    m.iter()
                        .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_string())))
                        .collect()
                })
                .unwrap_or_default(),
            setup: strings("setup"),
            teardown: strings("teardown"),
            toolchain: value
                .get("toolchain")
                .and_then(|t| serde_json::from_value(t.clone()).ok())
                .unwrap_or_default(),
            secrets: value
                .get("secrets")
                .and_then(|t| serde_json::from_value(t.clone()).ok())
                .unwrap_or_default(),
            gate: value
                .get("gate")
                .filter(|g| g.is_object())
                .map(|_| GatePolicy::from_recipe(Some(value))),
        }
    }
}

pub fn sha256(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub fn name_ok(name: &str) -> bool {
    let mut bytes = name.bytes();
    matches!(bytes.next(), Some(b) if b.is_ascii_uppercase() || b == b'_')
        && name.len() <= 64
        && bytes.all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
}

pub fn name_reserved(name: &str) -> bool {
    RESERVED_NAMES.contains(&name)
        || RESERVED_GO.contains(&name)
        || RESERVED_PREFIXES.iter().any(|p| name.starts_with(p))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    ports: Option<Spanned<i64>>,
    env: Option<Spanned<BTreeMap<String, Spanned<String>>>>,
    setup: Option<Spanned<Commands>>,
    teardown: Option<Spanned<Commands>>,
    toolchain: Option<RawToolchain>,
    secrets: Option<Spanned<BTreeMap<String, Spanned<RawSecret>>>>,
    gate: Option<RawGate>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawGate {
    source: Option<Spanned<String>>,
    commands: Option<Spanned<Vec<Spanned<String>>>>,
    required_checks: Option<Spanned<Vec<Spanned<String>>>>,
    timeout_minutes: Option<Spanned<i64>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSecret {
    to: Option<Vec<String>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawToolchain {
    go: Option<Spanned<String>>,
    node: Option<Spanned<String>>,
    pnpm: Option<Spanned<String>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Commands {
    commands: Vec<Spanned<String>>,
}

pub(crate) fn line_of(text: &str, offset: usize) -> usize {
    text.get(..offset)
        .map_or(1, |s| s.matches('\n').count() + 1)
}

fn issue(code: &'static str, field: impl Into<String>, text: &str, offset: usize) -> Issue {
    Issue {
        code,
        field: Some(field.into()),
        line: Some(line_of(text, offset)),
    }
}

fn commands(
    section: &'static str,
    raw: Option<Spanned<Commands>>,
    text: &str,
    issues: &mut Vec<Issue>,
) -> Vec<String> {
    let Some(raw) = raw else {
        return Vec::new();
    };
    let at = raw.span().start;
    let list = raw.into_inner().commands;
    if list.len() > MAX_COMMANDS {
        issues.push(issue(
            "too_many_commands",
            format!("{section}.commands"),
            text,
            at,
        ));
        return Vec::new();
    }
    let mut out = Vec::new();
    for (i, command) in list.into_iter().enumerate() {
        let field = format!("{section}.commands[{i}]");
        let start = command.span().start;
        let command = command.into_inner();
        if command.trim().is_empty() {
            issues.push(issue("command_empty", field, text, start));
        } else if command.len() > MAX_COMMAND_BYTES || command.contains('\0') {
            issues.push(issue("command_invalid", field, text, start));
        } else {
            out.push(command);
        }
    }
    out
}

fn secrets(
    raw: Option<Spanned<BTreeMap<String, Spanned<RawSecret>>>>,
    env: &BTreeMap<String, String>,
    text: &str,
    issues: &mut Vec<Issue>,
) -> BTreeMap<String, Vec<SecretPurpose>> {
    let mut out = BTreeMap::new();
    let Some(raw) = raw else {
        return out;
    };
    let at = raw.span().start;
    let declared = raw.into_inner();
    if declared.len() > MAX_SECRETS {
        issues.push(issue("too_many_secrets", "secrets", text, at));
        return out;
    }
    for (name, secret) in declared {
        let field = format!("secrets.{name}");
        let start = secret.span().start;
        let to = secret.into_inner().to;
        let purposes: Option<Vec<SecretPurpose>> = match &to {
            None => Some(vec![SecretPurpose::Setup]),
            Some(list) => list.iter().map(|p| p.parse().ok()).collect(),
        };
        if !name_ok(&name) {
            issues.push(issue("secret_name_invalid", field, text, start));
        } else if name_reserved(&name) {
            issues.push(issue("secret_name_reserved", field, text, start));
        } else if env.contains_key(&name) {
            issues.push(issue("secret_env_conflict", field, text, start));
        } else {
            match purposes {
                Some(mut p) if !p.is_empty() => {
                    p.sort();
                    p.dedup();
                    out.insert(name, p);
                }
                _ => issues.push(issue("secret_purpose_invalid", field, text, start)),
            }
        }
    }
    out
}

fn gate(raw: RawGate, text: &str, issues: &mut Vec<Issue>) -> GatePolicy {
    let mut policy = GatePolicy::default();
    if let Some(source) = raw.source {
        let at = source.span().start;
        match source.get_ref().parse::<GateMode>() {
            Ok(mode) => policy.mode = mode,
            Err(_) => issues.push(issue("gate_source_invalid", "gate.source", text, at)),
        }
    }
    if let Some(commands) = raw.commands {
        let at = commands.span().start;
        let list = commands.into_inner();
        if list.len() > MAX_GATE_COMMANDS {
            issues.push(issue("too_many_gate_commands", "gate.commands", text, at));
        }
        for (i, c) in list.into_iter().enumerate() {
            let start = c.span().start;
            let c = c.into_inner();
            if c.trim().is_empty() || c.len() > MAX_COMMAND_BYTES || c.contains('\0') {
                issues.push(issue(
                    "gate_command_invalid",
                    format!("gate.commands[{i}]"),
                    text,
                    start,
                ));
            } else {
                policy.commands.push(c);
            }
        }
    }
    if let Some(checks) = raw.required_checks {
        let at = checks.span().start;
        let list = checks.into_inner();
        if list.len() > MAX_REQUIRED_CHECKS {
            issues.push(issue(
                "too_many_required_checks",
                "gate.required_checks",
                text,
                at,
            ));
        }
        for (i, c) in list.into_iter().enumerate() {
            let start = c.span().start;
            let c = c.into_inner();
            if c.is_empty() || c.len() > MAX_CHECK_NAME_BYTES {
                issues.push(issue(
                    "gate_check_invalid",
                    format!("gate.required_checks[{i}]"),
                    text,
                    start,
                ));
            } else {
                policy.required_checks.push(c);
            }
        }
    }
    if let Some(minutes) = raw.timeout_minutes {
        let at = minutes.span().start;
        match u32::try_from(*minutes.get_ref()) {
            Ok(m) if (1..=240).contains(&m) => policy.timeout_minutes = m,
            _ => issues.push(issue(
                "gate_timeout_out_of_range",
                "gate.timeout_minutes",
                text,
                at,
            )),
        }
    }
    policy
}

pub fn parse(text: &str, max_ports: u16) -> Result<Recipe, Vec<Issue>> {
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
    let ports = raw.ports.and_then(|p| {
        let at = p.span().start;
        match u16::try_from(*p.get_ref()) {
            Ok(n) if n <= max_ports => Some(n),
            _ => {
                issues.push(issue("ports_out_of_range", "ports", text, at));
                None
            }
        }
    });
    let mut env = BTreeMap::new();
    if let Some(raw_env) = raw.env {
        let at = raw_env.span().start;
        let vars = raw_env.into_inner();
        if vars.len() > MAX_ENV_VARS {
            issues.push(issue("too_many_env", "env", text, at));
        }
        for (name, value) in vars {
            let field = format!("env.{name}");
            let start = value.span().start;
            let value = value.into_inner();
            if !name_ok(&name) {
                issues.push(issue("env_name_invalid", field, text, start));
            } else if name_reserved(&name) {
                issues.push(issue("env_name_reserved", field, text, start));
            } else if value.len() > MAX_ENV_VALUE_BYTES || value.contains('\0') {
                issues.push(issue("env_value_invalid", field, text, start));
            } else {
                env.insert(name, value);
            }
        }
    }
    let setup = commands("setup", raw.setup, text, &mut issues);
    let teardown = commands("teardown", raw.teardown, text, &mut issues);
    let mut toolchain = BTreeMap::new();
    if let Some(t) = raw.toolchain {
        for (tool, version) in [(Tool::Go, t.go), (Tool::Node, t.node), (Tool::Pnpm, t.pnpm)] {
            let Some(version) = version else { continue };
            let start = version.span().start;
            let version = version.into_inner();
            if version.trim().is_empty() || version.len() > 64 {
                issues.push(issue(
                    "toolchain_version_invalid",
                    format!("toolchain.{tool}"),
                    text,
                    start,
                ));
            } else {
                toolchain.insert(tool, version);
            }
        }
    }
    let secrets = secrets(raw.secrets, &env, text, &mut issues);
    let gate = raw.gate.map(|g| gate(g, text, &mut issues));
    if issues.is_empty() {
        Ok(Recipe {
            ports,
            env,
            setup,
            teardown,
            toolchain,
            secrets,
            gate,
        })
    } else {
        Err(issues)
    }
}

const RECIPE_HEADER: &str = "# Read by Saka from the default branch; each task runs these commands in its own environment.\n";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecipeAction {
    Create,
    None,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Note {
    pub code: &'static str,
    pub params: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RecipeProposal {
    pub action: RecipeAction,
    pub reason: Option<&'static str>,
    pub commands: Vec<String>,
    pub content: Option<String>,
    pub issues: Vec<Issue>,
    pub notes: Vec<Note>,
}

fn setup_unknown(lockfile: &str) -> Note {
    Note {
        code: "setup_command_unknown",
        params: json!({ "lockfile": lockfile }),
    }
}

fn setup_commands(files: &RepoFiles) -> (Vec<String>, Vec<Note>) {
    let mut commands = Vec::new();
    let mut notes = Vec::new();
    if files.has("pnpm-lock.yaml") {
        commands.push("pnpm install --frozen-lockfile");
    } else if files.has("package-lock.json") || files.has("npm-shrinkwrap.json") {
        commands.push("npm ci");
    } else {
        for lockfile in ["yarn.lock", "bun.lock", "bun.lockb"] {
            if files.has(lockfile) {
                notes.push(setup_unknown(lockfile));
            }
        }
    }
    if files.has("go.mod") && files.has("go.sum") {
        commands.push("go mod download");
    }
    if files.has("Cargo.lock") {
        commands.push("cargo fetch --locked");
    }
    if files.has("uv.lock") {
        commands.push("uv sync --frozen");
    } else if files.has("poetry.lock") {
        notes.push(setup_unknown("poetry.lock"));
    }
    (commands.into_iter().map(String::from).collect(), notes)
}

fn recipe_text(commands: &[String]) -> String {
    let list: Vec<String> = commands
        .iter()
        .map(|c| toml::Value::String(c.clone()).to_string())
        .collect();
    format!("{RECIPE_HEADER}[setup]\ncommands = [{}]\n", list.join(", "))
}

pub fn propose(files: &RepoFiles, max_ports: u16) -> RecipeProposal {
    let (commands, notes) = setup_commands(files);
    if files.has(RECIPE_PATH) {
        let issues = files
            .text(RECIPE_PATH)
            .map(|text| parse(text, max_ports).err().unwrap_or_default())
            .unwrap_or_default();
        return RecipeProposal {
            action: RecipeAction::None,
            reason: Some("recipe_exists"),
            commands: Vec::new(),
            content: None,
            issues,
            notes: Vec::new(),
        };
    }
    if commands.is_empty() {
        return RecipeProposal {
            action: RecipeAction::None,
            reason: Some("no_lockfile"),
            commands,
            content: None,
            issues: Vec::new(),
            notes,
        };
    }
    RecipeProposal {
        action: RecipeAction::Create,
        reason: None,
        content: Some(recipe_text(&commands)),
        commands,
        issues: Vec::new(),
        notes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn first(text: &str) -> (&'static str, Option<String>, Option<usize>) {
        let issue = parse(text, 10).unwrap_err().remove(0);
        (issue.code, issue.field, issue.line)
    }

    #[test]
    fn a_full_recipe_parses() {
        let recipe = parse(
            "ports = 3\n\n[env]\nAPP_ENV = \"test\"\n\n[setup]\ncommands = [\"pnpm install --frozen-lockfile\", \"make db-up\"]\n\n[teardown]\ncommands = [\"make db-down\"]\n\n[toolchain]\nnode = \"26.13.0\"\n",
            10,
        )
        .unwrap();
        assert_eq!(recipe.ports, Some(3));
        assert_eq!(recipe.env.get("APP_ENV").map(String::as_str), Some("test"));
        assert_eq!(
            recipe.setup,
            ["pnpm install --frozen-lockfile", "make db-up"]
        );
        assert_eq!(recipe.teardown, ["make db-down"]);
        assert_eq!(
            recipe.toolchain.get(&Tool::Node).map(String::as_str),
            Some("26.13.0")
        );
        assert_eq!(Recipe::from_json(&recipe.to_json()), recipe);
    }

    #[test]
    fn a_gate_table_parses_with_defaults_and_round_trips() {
        let recipe = parse(
            "[gate]\nsource = \"both\"\ncommands = [\"make lint\"]\nrequired_checks = [\"test\"]\ntimeout_minutes = 5\n",
            10,
        )
        .unwrap();
        let gate = recipe.gate.clone().unwrap();
        assert_eq!(gate.mode, GateMode::Both);
        assert_eq!(gate.commands, ["make lint"]);
        assert_eq!(gate.required_checks, ["test"]);
        assert_eq!(gate.timeout_minutes, 5);
        assert_eq!(Recipe::from_json(&recipe.to_json()), recipe);
        let empty = parse("[gate]\n", 10).unwrap();
        assert_eq!(empty.gate, Some(GatePolicy::default()));
        assert_eq!(parse("", 10).unwrap().gate, None);
    }

    #[test]
    fn a_bad_gate_names_the_field_and_line() {
        let many: String = (0..21).map(|i| format!("\"c{i}\",")).collect();
        let checks: String = (0..65).map(|i| format!("\"k{i}\",")).collect();
        for (text, code, field, line) in [
            (
                "[gate]\nsource = \"ci\"\n",
                "gate_source_invalid",
                "gate.source",
                2,
            ),
            (
                "[gate]\ncommands = [\" \"]\n",
                "gate_command_invalid",
                "gate.commands[0]",
                2,
            ),
            (
                &format!("[gate]\ncommands = [{many}]\n"),
                "too_many_gate_commands",
                "gate.commands",
                2,
            ),
            (
                "[gate]\nrequired_checks = [\"\"]\n",
                "gate_check_invalid",
                "gate.required_checks[0]",
                2,
            ),
            (
                &format!("[gate]\nrequired_checks = [{checks}]\n"),
                "too_many_required_checks",
                "gate.required_checks",
                2,
            ),
            (
                "[gate]\n\ntimeout_minutes = 0\n",
                "gate_timeout_out_of_range",
                "gate.timeout_minutes",
                3,
            ),
            (
                "[gate]\ntimeout_minutes = 241\n",
                "gate_timeout_out_of_range",
                "gate.timeout_minutes",
                2,
            ),
        ] {
            assert_eq!(
                first(text),
                (code, Some(field.to_string()), Some(line)),
                "{text}"
            );
        }
        assert_eq!(first("[gate]\nretries = 1\n").0, "field_unknown");
    }

    #[test]
    fn an_empty_file_is_an_empty_recipe() {
        assert_eq!(parse("", 10).unwrap(), Recipe::default());
    }

    #[test]
    fn unknown_fields_are_rejected_with_their_line() {
        assert_eq!(
            first("ports = 1\n[caches]\nNPM = 1\n"),
            ("field_unknown", Some("caches".into()), Some(2))
        );
        assert_eq!(
            first("[secrets]\nNPM_TOKEN = { to = [\"setup\"], file = true }\n").0,
            "field_unknown"
        );
        assert_eq!(first("[toolchain]\nrust = \"1.99.1\"\n").0, "field_unknown");
        assert_eq!(first("[setup]\ncommand = [\"x\"]\n").0, "field_unknown");
    }

    #[test]
    fn secrets_declare_their_purposes_and_default_to_setup() {
        let recipe = parse(
            "[secrets]\nNPM_TOKEN = {}\nTEST_API_KEY = { to = [\"session\", \"setup\", \"setup\"] }\n",
            10,
        )
        .unwrap();
        assert_eq!(recipe.secrets["NPM_TOKEN"], [SecretPurpose::Setup]);
        assert_eq!(
            recipe.secrets["TEST_API_KEY"],
            [SecretPurpose::Setup, SecretPurpose::Session]
        );
        assert_eq!(Recipe::from_json(&recipe.to_json()), recipe);
        for (text, code) in [
            ("[secrets]\nlower = {}\n", "secret_name_invalid"),
            ("[secrets]\nGIT_TOKEN = {}\n", "secret_name_reserved"),
            ("[secrets]\nPATH = {}\n", "secret_name_reserved"),
            (
                "[env]\nAPI = \"x\"\n[secrets]\nAPI = {}\n",
                "secret_env_conflict",
            ),
            ("[secrets]\nA_KEY = { to = [] }\n", "secret_purpose_invalid"),
            (
                "[secrets]\nA_KEY = { to = [\"ci\"] }\n",
                "secret_purpose_invalid",
            ),
        ] {
            assert_eq!(first(text).0, code, "{text}");
        }
        assert_eq!(
            first("\n[secrets]\nlower = {}\n"),
            ("secret_name_invalid", Some("secrets.lower".into()), Some(3))
        );
        let many: String = (0..=MAX_SECRETS)
            .map(|i| format!("S{i} = {{}}\n"))
            .collect();
        assert_eq!(first(&format!("[secrets]\n{many}")).0, "too_many_secrets");
    }

    #[test]
    fn ports_above_the_operator_block_are_rejected() {
        assert_eq!(
            first("ports = 11\n"),
            ("ports_out_of_range", Some("ports".into()), Some(1))
        );
        assert_eq!(first("ports = -1\n").0, "ports_out_of_range");
        assert_eq!(parse("ports = 0\n", 10).unwrap().ports, Some(0));
    }

    #[test]
    fn env_names_must_be_valid_and_not_owned_by_saka() {
        assert_eq!(
            first("[env]\nlower = \"x\"\n"),
            ("env_name_invalid", Some("env.lower".into()), Some(2))
        );
        for name in [
            "PATH",
            "HOME",
            "PORT",
            "SAKA_X",
            "GIT_DIR",
            "CLAUDE_CONFIG_DIR",
            "GOFLAGS",
            "npm_config_store_dir",
        ] {
            let text = format!("[env]\n\"{name}\" = \"x\"\n");
            let code = parse(&text, 10).unwrap_err().remove(0).code;
            assert!(
                code == "env_name_reserved" || code == "env_name_invalid",
                "{name}: {code}"
            );
        }
        assert_eq!(first("[env]\nPATH = \"/x\"\n").0, "env_name_reserved");
    }

    #[test]
    fn command_lists_are_bounded() {
        let many = (0..21)
            .map(|i| format!("\"c{i}\""))
            .collect::<Vec<_>>()
            .join(", ");
        assert_eq!(
            first(&format!("[setup]\ncommands = [{many}]\n")).0,
            "too_many_commands"
        );
        let long = "x".repeat(MAX_COMMAND_BYTES + 1);
        assert_eq!(
            first(&format!("[teardown]\ncommands = [\"{long}\"]\n")),
            (
                "command_invalid",
                Some("teardown.commands[0]".into()),
                Some(2)
            )
        );
        assert_eq!(first("[setup]\ncommands = [\"  \"]\n").0, "command_empty");
    }

    #[test]
    fn broken_toml_is_reported_as_such() {
        assert_eq!(first("ports = \n").0, "toml_invalid");
        assert_eq!(first("ports = \"3\"\n").0, "field_invalid");
    }
}
