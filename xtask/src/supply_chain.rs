use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;

use crate::check::step;
use crate::metadata;
use crate::tools::{self, Installer};

const RUST_FIXTURES: &str = "xtask/fixtures/supply-chain";

pub fn run(offline: bool) -> Result<()> {
    let root = metadata::workspace_root()?;
    let meta = metadata::load()?;
    step("supply chain (Rust)", || rust(&root, &meta, offline))?;
    step("supply chain (web)", || {
        crate::supply_chain_web::web(&root, offline)
    })?;
    println!("supply-chain: ok");
    Ok(())
}

pub fn rust(root: &Path, meta: &Value, offline: bool) -> Result<()> {
    let target_dir = metadata::target_directory(meta)?;
    let installer = Installer::new(&target_dir, offline);
    let tool = |name: &str| -> Result<PathBuf> {
        let version = tools::pinned(meta, name)?;
        installer.install(name, name, &version, name)
    };
    let rust = Rust {
        root: root.to_path_buf(),
        deny: tool("cargo-deny")?,
        shear: tool("cargo-shear")?,
        fixtures: target_dir.join("xtask-fixtures").join("supply-chain"),
        offline,
    };
    let spikes = metadata::spike_packages(meta)?;
    rust.workspace_deny(&spikes)?;
    rust.workspace_shear(&spikes)?;
    rust.fixtures()
}

struct Rust {
    root: PathBuf,
    deny: PathBuf,
    shear: PathBuf,
    fixtures: PathBuf,
    offline: bool,
}

impl Rust {
    fn deny_command(&self, manifest: &Path, config: &Path, json: bool) -> Command {
        let mut cmd = Command::new(&self.deny);
        cmd.arg("--locked");
        if self.offline {
            cmd.arg("--offline");
        }
        if json {
            cmd.args(["--format", "json"]);
        }
        cmd.arg("--manifest-path")
            .arg(manifest)
            .arg("--config")
            .arg(config)
            .current_dir(&self.root);
        cmd
    }

    fn workspace_deny(&self, spikes: &[String]) -> Result<()> {
        let mut cmd = self.deny_command(
            &self.root.join("Cargo.toml"),
            &self.root.join("deny.toml"),
            false,
        );
        for spike in spikes {
            cmd.args(["--exclude", spike]);
        }
        cmd.args([
            "check",
            "-D",
            "unmatched-skip",
            "advisories",
            "bans",
            "licenses",
            "sources",
        ]);
        run_status(&mut cmd, "cargo-deny check")
    }

    fn workspace_shear(&self, spikes: &[String]) -> Result<()> {
        let mut cmd = self.shear_command();
        for spike in spikes {
            cmd.args(["--exclude", spike]);
        }
        cmd.arg(&self.root);
        run_status(&mut cmd, "cargo-shear")
    }

    fn shear_command(&self) -> Command {
        let mut cmd = Command::new(&self.shear);
        cmd.args(["--locked", "--deny-warnings"]);
        if self.offline {
            cmd.arg("--offline");
        }
        cmd.current_dir(&self.root);
        cmd
    }

    fn fixture_deny(&self, fixture: &str, check: &str, expected: &[(&str, &str)]) -> Result<()> {
        let manifest = self
            .root
            .join(RUST_FIXTURES)
            .join(fixture)
            .join("Cargo.toml");
        self.expect_deny_rejects(
            fixture,
            &manifest,
            &self.root.join("deny.toml"),
            check,
            expected,
        )
    }

    fn expect_deny_rejects(
        &self,
        fixture: &str,
        manifest: &Path,
        config: &Path,
        check: &str,
        expected: &[(&str, &str)],
    ) -> Result<()> {
        let out = self
            .deny_command(manifest, config, true)
            .args(["check", check])
            .output()
            .with_context(|| format!("run cargo-deny on the {fixture} fixture"))?;
        let stderr = String::from_utf8_lossy(&out.stderr);
        expect_rejected(
            fixture,
            out.status.success(),
            &deny_findings(&stderr),
            expected,
        )
        .map_err(|err| anyhow!("{err}\n{}", stderr.trim()))
    }

    fn fixtures(&self) -> Result<()> {
        self.fixture_deny(
            "license",
            "licenses",
            &[("rejected", "copyleft-dep"), ("rejected", "mixed-dep")],
        )?;
        self.fixture_deny(
            "bans",
            "bans",
            &[("banned", "openssl-sys"), ("duplicate", "dup")],
        )?;
        self.fixture_deny(
            "advisory",
            "advisories",
            &[("vulnerability", "RUSTSEC-2021-0003")],
        )?;
        self.git_source_fixture()?;
        self.unused_dep_fixture()?;
        self.missing_advisory_db_fixture()
    }

    fn git_source_fixture(&self) -> Result<()> {
        let dir = self.fixtures.join("git-source");
        if dir.exists() {
            std::fs::remove_dir_all(&dir).with_context(|| format!("clear {}", dir.display()))?;
        }
        let upstream = dir.join("upstream");
        let app = dir.join("app");
        write_crate(&upstream, "git-dep", "")?;
        for args in [
            &["init", "--quiet", "--initial-branch", "main"][..],
            &["add", "--all"],
            &["commit", "--quiet", "--message", "git dependency"],
        ] {
            run_output(
                fixture_git(&upstream).args(args),
                "git (git-source fixture)",
            )?;
        }
        let url = format!("file://{}", upstream.display());
        write_crate(
            &app,
            "git-source-fixture",
            &format!("\n[dependencies]\ngit-dep = {{ git = \"{url}\" }}\n\n[workspace]\n"),
        )?;
        run_output(
            Command::new(metadata::cargo())
                .args(["generate-lockfile", "--quiet"])
                .current_dir(&app),
            "cargo generate-lockfile (git-source fixture)",
        )?;
        self.expect_deny_rejects(
            "git-source",
            &app.join("Cargo.toml"),
            &self.root.join("deny.toml"),
            "sources",
            &[("source-not-allowed", "git-dep")],
        )
    }

    fn unused_dep_fixture(&self) -> Result<()> {
        let fixture = self.root.join(RUST_FIXTURES).join("unused-dep");
        let out = self
            .shear_command()
            .args(["--format", "json"])
            .arg(&fixture)
            .output()
            .context("run cargo-shear on the unused-dep fixture")?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        let findings = shear_findings(&stdout)
            .map_err(|err| anyhow!("{err:#}\n{}", String::from_utf8_lossy(&out.stderr).trim()))?;
        expect_rejected(
            "unused-dep",
            out.status.success(),
            &findings,
            &[("shear/unused_dependency", "unused-helper")],
        )
        .map_err(|err| anyhow!("{err}\n{}", stdout.trim()))
    }

    fn missing_advisory_db_fixture(&self) -> Result<()> {
        let dir = self.fixtures.join("missing-advisory-db");
        let empty_db = dir.join("db");
        if dir.exists() {
            std::fs::remove_dir_all(&dir).with_context(|| format!("clear {}", dir.display()))?;
        }
        std::fs::create_dir_all(&empty_db)
            .with_context(|| format!("create {}", empty_db.display()))?;
        let config = dir.join("deny.toml");
        let policy =
            std::fs::read_to_string(self.root.join("deny.toml")).context("read deny.toml")?;
        std::fs::write(&config, with_db_path(&policy, &empty_db)?)
            .with_context(|| format!("write {}", config.display()))?;
        let manifest = self
            .root
            .join(RUST_FIXTURES)
            .join("advisory")
            .join("Cargo.toml");
        let out = Command::new(&self.deny)
            .args([
                "--locked",
                "--offline",
                "--format",
                "json",
                "--manifest-path",
            ])
            .arg(&manifest)
            .arg("--config")
            .arg(&config)
            .args(["check", "advisories"])
            .current_dir(&self.root)
            .output()
            .context("run cargo-deny offline with an empty advisory database")?;
        let stderr = String::from_utf8_lossy(&out.stderr);
        if out.status.success() {
            bail!("cargo-deny --offline passed with an empty advisory database\n{stderr}");
        }
        if !stderr.contains(&empty_db.display().to_string()) {
            bail!(
                "cargo-deny --offline failed for another reason than the empty advisory database\n{stderr}"
            );
        }
        Ok(())
    }
}

fn with_db_path(policy: &str, db_path: &Path) -> Result<String> {
    let mut config: toml::Table = toml::from_str(policy).context("parse deny.toml")?;
    let advisories = config
        .get_mut("advisories")
        .and_then(toml::Value::as_table_mut)
        .context("deny.toml has no [advisories] table")?;
    advisories.insert(
        "db-path".to_string(),
        toml::Value::String(db_path.display().to_string()),
    );
    toml::to_string(&config).context("serialize deny.toml")
}

fn write_crate(dir: &Path, name: &str, tail: &str) -> Result<()> {
    std::fs::create_dir_all(dir.join("src"))
        .with_context(|| format!("create {}", dir.display()))?;
    std::fs::write(
        dir.join("Cargo.toml"),
        format!(
            "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2024\"\nlicense = \"MIT\"\npublish = false\n{tail}"
        ),
    )?;
    std::fs::write(dir.join("src/lib.rs"), "pub fn fixture() {}\n")?;
    Ok(())
}

fn fixture_git(dir: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
        .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z")
        .env("GIT_COMMITTER_NAME", "fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
        .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z")
        .current_dir(dir);
    cmd
}

fn run_status(cmd: &mut Command, what: &str) -> Result<()> {
    let status = cmd.status().with_context(|| format!("run {what}"))?;
    if !status.success() {
        bail!("{what} exited with {status}");
    }
    Ok(())
}

fn run_output(cmd: &mut Command, what: &str) -> Result<Output> {
    let out = cmd.output().with_context(|| format!("run {what}"))?;
    if !out.status.success() {
        bail!(
            "{what} exited with {}\n{}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(out)
}

#[derive(Debug)]
pub(crate) struct Finding {
    pub(crate) code: String,
    pub(crate) subjects: Vec<String>,
}

fn deny_findings(stderr: &str) -> Vec<Finding> {
    stderr
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|event| event["type"] == "diagnostic" && event["fields"]["severity"] == "error")
        .filter_map(|event| {
            let code = event["fields"]["code"].as_str()?.to_string();
            let mut subjects = Vec::new();
            string_leaves(&event["fields"], &mut subjects);
            Some(Finding { code, subjects })
        })
        .collect()
}

fn string_leaves(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(s) => out.push(s.clone()),
        Value::Array(items) => items.iter().for_each(|v| string_leaves(v, out)),
        Value::Object(map) => map.values().for_each(|v| string_leaves(v, out)),
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn shear_findings(stdout: &str) -> Result<Vec<Finding>> {
    let report: Value = serde_json::from_str(stdout).context("parse cargo-shear JSON report")?;
    let findings = report["findings"]
        .as_array()
        .context("cargo-shear report has no findings array")?;
    Ok(findings
        .iter()
        .filter(|finding| finding["severity"] == "error")
        .filter_map(|finding| {
            let code = finding["code"].as_str()?.to_string();
            let subjects = finding["message"]
                .as_str()?
                .split('`')
                .skip(1)
                .step_by(2)
                .map(str::to_string)
                .collect();
            Some(Finding { code, subjects })
        })
        .collect())
}

pub(crate) fn expect_rejected(
    fixture: &str,
    accepted: bool,
    findings: &[Finding],
    expected: &[(&str, &str)],
) -> Result<()> {
    let wanted = || {
        expected
            .iter()
            .map(|(code, subject)| format!("{code} ({subject})"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    if accepted {
        bail!(
            "the {fixture} fixture was accepted; expected rejection with {}",
            wanted()
        );
    }
    let missing: Vec<String> = expected
        .iter()
        .filter(|(code, subject)| {
            !findings
                .iter()
                .any(|f| f.code == *code && f.subjects.iter().any(|s| s == subject))
        })
        .map(|(code, subject)| format!("{code} ({subject})"))
        .collect();
    if !missing.is_empty() {
        bail!(
            "the {fixture} fixture was not rejected with {}",
            missing.join(", ")
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const DENY_STDERR: &str = r#"{"fields":{"code":"rejected","graphs":[{"Krate":{"name":"copyleft-dep","version":"0.1.0"}}],"message":"failed to satisfy license requirements","severity":"error"},"type":"diagnostic"}
{"fields":{"code":"vulnerability","advisory":{"id":"RUSTSEC-2021-0003"},"graphs":[{"Krate":{"name":"smallvec","version":"1.6.0"}}],"severity":"error"},"type":"diagnostic"}
{"fields":{"code":"duplicate","graphs":[{"Krate":{"name":"dup","version":"1.0.0"}}],"severity":"warning"},"type":"diagnostic"}
{"fields":{"level":"ERROR","message":"rejected copyleft-dep"},"type":"log"}
not json
{"fields":{"advisories":{"errors":1}},"type":"summary"}"#;

    #[test]
    fn deny_diagnostics_carry_their_code_and_subjects() {
        let findings = deny_findings(DENY_STDERR);
        assert_eq!(findings.len(), 2);
        assert!(expect_rejected("f", false, &findings, &[("rejected", "copyleft-dep")]).is_ok());
        assert!(
            expect_rejected(
                "f",
                false,
                &findings,
                &[("vulnerability", "RUSTSEC-2021-0003")]
            )
            .is_ok()
        );
    }

    #[test]
    fn an_accepted_fixture_fails_the_step() {
        let findings = deny_findings(DENY_STDERR);
        let err = expect_rejected("license", true, &findings, &[("rejected", "copyleft-dep")])
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("license") && err.contains("rejected (copyleft-dep)"),
            "{err}"
        );
    }

    #[test]
    fn a_missing_diagnostic_fails_the_step_and_names_fixture_and_code() {
        let findings = deny_findings(DENY_STDERR);
        let err = expect_rejected(
            "license",
            false,
            &findings,
            &[("rejected", "copyleft-dep"), ("rejected", "mixed-dep")],
        )
        .unwrap_err()
        .to_string();
        assert_eq!(
            err,
            "the license fixture was not rejected with rejected (mixed-dep)"
        );
        let wrong_code = expect_rejected("bans", false, &findings, &[("banned", "copyleft-dep")]);
        assert!(wrong_code.is_err());
        let log_only = expect_rejected(
            "bans",
            false,
            &findings,
            &[("rejected", "rejected copyleft-dep")],
        );
        assert!(log_only.is_err());
        let only_warned = expect_rejected("bans", false, &findings, &[("duplicate", "dup")]);
        assert!(only_warned.is_err());
    }

    #[test]
    fn shear_findings_name_the_backticked_dependency() {
        let report = r#"{"summary":{"errors":1},"findings":[{"code":"shear/unused_dependency","severity":"error","message":"unused dependency `unused-helper`"},{"code":"shear/unused_dependency","severity":"warning","message":"unused dependency `warned`"}]}"#;
        let findings = shear_findings(report).unwrap();
        assert!(
            expect_rejected(
                "unused-dep",
                false,
                &findings,
                &[("shear/unused_dependency", "unused-helper")]
            )
            .is_ok()
        );
        assert!(
            expect_rejected(
                "unused-dep",
                false,
                &findings,
                &[("shear/unused_dependency", "other")]
            )
            .is_err()
        );
        assert!(
            expect_rejected(
                "unused-dep",
                false,
                &findings,
                &[("shear/unused_dependency", "warned")]
            )
            .is_err()
        );
        assert!(shear_findings("").is_err());
    }

    #[test]
    fn the_offline_database_config_only_moves_the_database() {
        let root = metadata::workspace_root().unwrap();
        let policy = std::fs::read_to_string(root.join("deny.toml")).unwrap();
        let moved: toml::Table =
            toml::from_str(&with_db_path(&policy, Path::new("/empty")).unwrap()).unwrap();
        let mut original: toml::Table = toml::from_str(&policy).unwrap();
        original["advisories"].as_table_mut().unwrap().insert(
            "db-path".to_string(),
            toml::Value::String("/empty".to_string()),
        );
        assert_eq!(moved, original);
    }

    fn policy() -> toml::Table {
        let root = metadata::workspace_root().unwrap();
        toml::from_str(&std::fs::read_to_string(root.join("deny.toml")).unwrap()).unwrap()
    }

    fn strings(value: &toml::Value) -> Vec<&str> {
        value
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect()
    }

    #[test]
    fn the_policy_fails_on_every_advisory_kind() {
        let policy = policy();
        let advisories = &policy["advisories"];
        assert_eq!(advisories["yanked"].as_str(), Some("deny"));
        assert_eq!(advisories["unmaintained"].as_str(), Some("all"));
        assert_eq!(advisories["unsound"].as_str(), Some("all"));
        assert_eq!(advisories["unused-ignored-advisory"].as_str(), Some("deny"));
        assert_eq!(advisories["maximum-db-staleness"].as_str(), Some("P7D"));
        assert!(advisories.get("db-path").is_none());
        for ignore in advisories["ignore"].as_array().unwrap() {
            assert!(
                ignore["id"].as_str().unwrap().starts_with("RUSTSEC-"),
                "{ignore}"
            );
            assert!(
                !ignore["reason"].as_str().unwrap().trim().is_empty(),
                "{ignore}"
            );
        }
    }

    const FILE_LEVEL_COPYLEFT: &[&str] = &["MPL-2.0"];

    #[test]
    fn only_permissive_non_copyleft_licences_are_allowed_for_shipped_crates() {
        let policy = policy();
        let licenses = &policy["licenses"];
        assert_eq!(licenses["include-dev"].as_bool(), Some(false));
        assert_eq!(licenses["unused-license-exception"].as_str(), Some("deny"));
        assert_eq!(licenses["private"]["ignore"].as_bool(), Some(false));
        let allow = strings(&licenses["allow"]);
        assert!(!allow.is_empty());
        for id in allow {
            let license = spdx::license_id(id).unwrap_or_else(|| panic!("{id} is not an SPDX id"));
            assert!(!license.is_copyleft(), "{id} is copyleft");
            assert!(
                license.is_osi_approved() || license.is_fsf_free_libre(),
                "{id} is neither OSI approved nor FSF libre"
            );
        }
        for exception in licenses["exceptions"].as_array().unwrap() {
            for id in strings(&exception["allow"]) {
                let license =
                    spdx::license_id(id).unwrap_or_else(|| panic!("{id} is not an SPDX id"));
                assert!(
                    !license.is_copyleft() || FILE_LEVEL_COPYLEFT.contains(&id),
                    "{exception}"
                );
            }
        }
    }

    #[test]
    fn duplicates_wildcards_and_openssl_are_banned_and_every_skip_has_a_reason() {
        let policy = policy();
        let bans = &policy["bans"];
        assert_eq!(bans["multiple-versions"].as_str(), Some("deny"));
        assert_eq!(bans["multiple-versions-include-dev"].as_bool(), Some(false));
        assert_eq!(bans["wildcards"].as_str(), Some("deny"));
        assert_eq!(bans["allow-wildcard-paths"].as_bool(), Some(true));
        let denied: Vec<&str> = bans["deny"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| {
                assert!(
                    !entry["reason"].as_str().unwrap().trim().is_empty(),
                    "{entry}"
                );
                entry["crate"].as_str().unwrap()
            })
            .collect();
        assert_eq!(denied, ["openssl", "openssl-sys", "native-tls"]);
        for skip in bans["skip"].as_array().unwrap() {
            assert!(skip["crate"].as_str().unwrap().contains('@'), "{skip}");
            assert!(
                !skip["reason"].as_str().unwrap().trim().is_empty(),
                "{skip}"
            );
        }
        assert!(bans.get("skip-tree").is_none());
        let workspace = &bans["workspace-dependencies"];
        assert_eq!(workspace["duplicates"].as_str(), Some("deny"));
        assert_eq!(workspace["unused"].as_str(), Some("deny"));
        assert_eq!(
            workspace["include-path-dependencies"].as_bool(),
            Some(false)
        );
    }

    #[test]
    fn crates_come_only_from_crates_io() {
        let policy = policy();
        let sources = &policy["sources"];
        assert_eq!(sources["unknown-registry"].as_str(), Some("deny"));
        assert_eq!(sources["unknown-git"].as_str(), Some("deny"));
        assert_eq!(
            strings(&sources["allow-registry"]),
            ["https://github.com/rust-lang/crates.io-index"]
        );
        assert!(sources["allow-git"].as_array().unwrap().is_empty());
        assert!(sources.get("allow-org").is_none());
    }
}
