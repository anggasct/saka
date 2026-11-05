use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;

use crate::supply_chain::{Finding, expect_rejected};

const FIXTURES: &str = "xtask/fixtures/supply-chain";

const POLICY: &[(&str, &str)] = &[
    ("minimumReleaseAge", "1440"),
    ("trustPolicy", "no-downgrade"),
    ("blockExoticSubdeps", "true"),
    ("strictDepBuilds", "true"),
];

pub fn web(root: &Path, offline: bool) -> Result<()> {
    let web = Web {
        root: root.to_path_buf(),
    };
    web.policy()?;
    web.licenses()?;
    if offline {
        println!("skipped (offline): pnpm audit, pnpm audit signatures");
    } else {
        web.audit()?;
        web.signatures()?;
    }
    web.unused()?;
    web.fixtures(offline)
}

struct Web {
    root: PathBuf,
}

impl Web {
    fn dir(&self) -> PathBuf {
        self.root.join("web")
    }

    fn fixture(&self, name: &str) -> PathBuf {
        self.root.join(FIXTURES).join(name)
    }

    fn pnpm(&self, dir: &Path, args: &[&str]) -> Command {
        let mut cmd = Command::new("corepack");
        cmd.arg("pnpm")
            .args(args)
            .current_dir(dir)
            .env("CI", "true");
        cmd
    }

    fn output(&self, dir: &Path, args: &[&str]) -> Result<Output> {
        self.pnpm(dir, args)
            .output()
            .with_context(|| format!("run corepack pnpm {} (is corepack installed?)", args[0]))
    }

    fn passes(&self, args: &[&str]) -> Result<()> {
        let status = self
            .pnpm(&self.dir(), args)
            .status()
            .with_context(|| format!("run corepack pnpm {} (is corepack installed?)", args[0]))?;
        if !status.success() {
            bail!("pnpm {} exited with {status}", args.join(" "));
        }
        Ok(())
    }

    fn policy(&self) -> Result<()> {
        for (key, expected) in POLICY {
            let out = self.output(&self.dir(), &["config", "get", key])?;
            check_policy(key, &String::from_utf8_lossy(&out.stdout), expected)?;
        }
        Ok(())
    }

    fn licenses(&self) -> Result<()> {
        let out = self.output(&self.dir(), &["licenses", "list", "--prod", "--json"])?;
        if !out.status.success() {
            bail!(
                "pnpm licenses list exited with {}\n{}",
                out.status,
                String::from_utf8_lossy(&out.stderr)
            );
        }
        let report: Value =
            serde_json::from_slice(&out.stdout).context("parse pnpm licenses report")?;
        let allow = allowed_licenses(&self.root.join("deny.toml"))?;
        let exceptions = license_exceptions(&self.dir().join("license-exceptions.toml"))?;
        check_licenses(&report, &allow, &exceptions)
    }

    fn audit(&self) -> Result<()> {
        self.passes(&["audit", "--audit-level", "low"])
    }

    fn signatures(&self) -> Result<()> {
        self.passes(&["audit", "signatures"])
    }

    fn unused(&self) -> Result<()> {
        self.passes(&["deps:check"])
    }

    fn fixtures(&self, offline: bool) -> Result<()> {
        if !offline {
            self.audit_fixture()?;
            self.signatures_fixture()?;
        }
        self.unused_fixture()
    }

    fn audit_fixture(&self) -> Result<()> {
        let out = self.output(
            &self.fixture("web-audit"),
            &["audit", "--audit-level", "low", "--json"],
        )?;
        let findings = audit_findings(&String::from_utf8_lossy(&out.stdout));
        expect_rejected(
            "web-audit",
            out.status.success(),
            &findings,
            &[("vulnerability", "GHSA-35jh-r3h4-6jhm")],
        )
        .map_err(|err| with_output(err, &out))
    }

    fn signatures_fixture(&self) -> Result<()> {
        let out = self.output(
            &self.fixture("web-signatures"),
            &["audit", "signatures", "--json"],
        )?;
        let findings = signature_findings(&String::from_utf8_lossy(&out.stdout));
        expect_rejected(
            "web-signatures",
            out.status.success(),
            &findings,
            &[("invalid-signature", "left-pad")],
        )
        .map_err(|err| with_output(err, &out))
    }

    fn unused_fixture(&self) -> Result<()> {
        let fixture = self.fixture("web-unused");
        let fixture = fixture.to_string_lossy();
        let out = self.output(
            &self.dir(),
            &[
                "exec",
                "knip",
                "--dependencies",
                "--directory",
                &fixture,
                "--reporter",
                "json",
            ],
        )?;
        let findings = unused_findings(&String::from_utf8_lossy(&out.stdout));
        expect_rejected(
            "web-unused",
            out.status.success(),
            &findings,
            &[("unused-dependency", "left-pad")],
        )
        .map_err(|err| with_output(err, &out))
    }
}

fn with_output(err: anyhow::Error, out: &Output) -> anyhow::Error {
    anyhow!(
        "{err}\n{}\n{}",
        String::from_utf8_lossy(&out.stdout).trim(),
        String::from_utf8_lossy(&out.stderr).trim()
    )
}

pub fn check_policy(key: &str, actual: &str, expected: &str) -> Result<()> {
    let actual = actual.trim();
    if actual == expected {
        Ok(())
    } else {
        bail!("pnpm {key} is `{actual}`, expected `{expected}` (web/pnpm-workspace.yaml)")
    }
}

fn audit_findings(stdout: &str) -> Vec<Finding> {
    let Ok(report) = serde_json::from_str::<Value>(stdout) else {
        return Vec::new();
    };
    report["advisories"]
        .as_object()
        .into_iter()
        .flat_map(|advisories| advisories.values())
        .map(|advisory| Finding {
            code: "vulnerability".to_string(),
            subjects: ["github_advisory_id", "module_name"]
                .iter()
                .filter_map(|key| advisory[key].as_str().map(str::to_string))
                .collect(),
        })
        .collect()
}

fn signature_findings(stdout: &str) -> Vec<Finding> {
    let Ok(report) = serde_json::from_str::<Value>(stdout) else {
        return Vec::new();
    };
    report["invalid"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|package| {
            Some(Finding {
                code: "invalid-signature".to_string(),
                subjects: vec![package["name"].as_str()?.to_string()],
            })
        })
        .collect()
}

fn unused_findings(stdout: &str) -> Vec<Finding> {
    let Ok(report) = serde_json::from_str::<Value>(stdout) else {
        return Vec::new();
    };
    report["issues"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|issue| {
            ["dependencies", "devDependencies"]
                .into_iter()
                .flat_map(|kind| issue[kind].as_array().into_iter().flatten())
        })
        .filter_map(|dependency| {
            Some(Finding {
                code: "unused-dependency".to_string(),
                subjects: vec![dependency["name"].as_str()?.to_string()],
            })
        })
        .collect()
}

#[derive(Debug)]
pub struct LicenseException {
    package: String,
    license: String,
}

fn allowed_licenses(deny: &Path) -> Result<BTreeSet<String>> {
    let policy: toml::Table = toml::from_str(
        &std::fs::read_to_string(deny).with_context(|| format!("read {}", deny.display()))?,
    )
    .context("parse deny.toml")?;
    policy
        .get("licenses")
        .and_then(|licenses| licenses.get("allow"))
        .and_then(toml::Value::as_array)
        .context("deny.toml has no [licenses] allow list")?
        .iter()
        .map(|id| {
            id.as_str()
                .map(str::to_string)
                .context("deny.toml [licenses] allow holds a non-string entry")
        })
        .collect()
}

fn license_exceptions(path: &Path) -> Result<Vec<LicenseException>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    parse_exceptions(&text).with_context(|| format!("{}", path.display()))
}

pub fn parse_exceptions(text: &str) -> Result<Vec<LicenseException>> {
    let table: toml::Table = toml::from_str(text).context("parse licence exceptions")?;
    let Some(entries) = table.get("exception") else {
        return Ok(Vec::new());
    };
    let entries = entries
        .as_array()
        .context("`exception` must be an array of tables")?;
    entries
        .iter()
        .map(|entry| {
            let field = |name: &str| {
                entry
                    .get(name)
                    .and_then(toml::Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string)
                    .with_context(|| format!("an exception needs a non-empty `{name}`"))
            };
            let package = field("package")?;
            let license = field("license")?;
            field("reason")?;
            if !package.contains('@') {
                bail!("exception package `{package}` must be name@version");
            }
            if spdx::license_id(&license).is_none() {
                bail!("exception for {package}: `{license}` is not an SPDX licence id");
            }
            Ok(LicenseException { package, license })
        })
        .collect()
}

pub fn check_licenses(
    report: &Value,
    allow: &BTreeSet<String>,
    exceptions: &[LicenseException],
) -> Result<()> {
    let groups = report
        .as_object()
        .context("pnpm licenses report is not an object")?;
    let mut violations = Vec::new();
    let mut matched = BTreeSet::new();
    for (group, packages) in groups {
        for package in packages.as_array().into_iter().flatten() {
            let name = package["name"].as_str().unwrap_or("<unnamed>");
            let license = package["license"].as_str().unwrap_or(group);
            for version in package["versions"].as_array().into_iter().flatten() {
                let id = format!("{name}@{}", version.as_str().unwrap_or("?"));
                let mut permitted = allow.clone();
                for exception in exceptions.iter().filter(|e| e.package == id) {
                    permitted.insert(exception.license.clone());
                    matched.insert(exception.package.as_str());
                }
                if let Err(reason) = satisfied(license, &permitted) {
                    violations.push(format!("{id}: {license:?} ({reason})"));
                }
            }
        }
    }
    let unmatched: Vec<&str> = exceptions
        .iter()
        .map(|e| e.package.as_str())
        .filter(|package| !matched.contains(package))
        .collect();
    if violations.is_empty() && unmatched.is_empty() {
        return Ok(());
    }
    let mut message = String::new();
    for violation in &violations {
        message.push_str(&format!("  {violation}\n"));
    }
    for package in unmatched {
        message.push_str(&format!(
            "  web/license-exceptions.toml: no production package matches {package}\n"
        ));
    }
    bail!("web licence check failed:\n{}", message.trim_end())
}

fn satisfied(license: &str, permitted: &BTreeSet<String>) -> std::result::Result<(), String> {
    let expression = spdx::Expression::parse(license).map_err(|err| format!("not SPDX: {err}"))?;
    if expression.evaluate(|req| match &req.license {
        spdx::LicenseItem::Spdx { id, .. } => permitted.contains(id.name),
        spdx::LicenseItem::Other(_) => false,
    }) {
        Ok(())
    } else {
        Err("not covered by the allow-list".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata;

    fn allow() -> BTreeSet<String> {
        allowed_licenses(&metadata::workspace_root().unwrap().join("deny.toml")).unwrap()
    }

    fn recorded() -> Value {
        let path = metadata::workspace_root()
            .unwrap()
            .join(FIXTURES)
            .join("pnpm-licenses.json");
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    fn only(report: &Value, name: &str) -> Value {
        let mut kept = serde_json::Map::new();
        for (group, packages) in report.as_object().unwrap() {
            let matching: Vec<Value> = packages
                .as_array()
                .unwrap()
                .iter()
                .filter(|p| p["name"] == name)
                .cloned()
                .collect();
            if !matching.is_empty() {
                kept.insert(group.clone(), Value::Array(matching));
            }
        }
        Value::Object(kept)
    }

    fn exception(package: &str, license: &str) -> LicenseException {
        LicenseException {
            package: package.to_string(),
            license: license.to_string(),
        }
    }

    #[test]
    fn permitted_expressions_pass() {
        let report = recorded();
        for name in ["left-pad", "dual", "either-pkg"] {
            check_licenses(&only(&report, name), &allow(), &[]).unwrap();
        }
    }

    #[test]
    fn copyleft_non_spdx_and_blank_licences_are_rejected_by_name_and_version() {
        let report = recorded();
        for (name, id) in [
            ("copyleft-pkg", "copyleft-pkg@3.1.0"),
            ("both-pkg", "both-pkg@4.2.0"),
            ("free-pkg", "free-pkg@0.9.0"),
            ("blank-pkg", "blank-pkg@0.1.0"),
        ] {
            let err = check_licenses(&only(&report, name), &allow(), &[])
                .unwrap_err()
                .to_string();
            assert!(err.contains(id), "{err}");
        }
        let err = check_licenses(&report, &allow(), &[])
            .unwrap_err()
            .to_string();
        assert!(
            !err.contains("left-pad") && !err.contains("either-pkg"),
            "{err}"
        );
    }

    #[test]
    fn an_exception_clears_exactly_its_package_and_version() {
        let report = only(&recorded(), "copyleft-pkg");
        let wrong_version = [exception("copyleft-pkg@9.9.9", "GPL-3.0-only")];
        assert!(check_licenses(&report, &allow(), &wrong_version).is_err());
        let exact = [exception("copyleft-pkg@3.1.0", "GPL-3.0-only")];
        check_licenses(&report, &allow(), &exact).unwrap();
        let other = recorded();
        let err = check_licenses(&other, &allow(), &exact)
            .unwrap_err()
            .to_string();
        assert!(!err.contains("copyleft-pkg"), "{err}");
        assert!(err.contains("both-pkg@4.2.0"), "{err}");
    }

    #[test]
    fn an_exception_for_one_version_leaves_the_other_rejected() {
        let report = serde_json::json!({
            "GPL-3.0-only": [{
                "name": "twin", "versions": ["1.0.0", "2.0.0"], "license": "GPL-3.0-only"
            }]
        });
        let err = check_licenses(
            &report,
            &allow(),
            &[exception("twin@1.0.0", "GPL-3.0-only")],
        )
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("twin@2.0.0") && !err.contains("twin@1.0.0:"),
            "{err}"
        );
    }

    #[test]
    fn an_exception_that_matches_no_package_fails_the_check() {
        let report = only(&recorded(), "left-pad");
        let err = check_licenses(&report, &allow(), &[exception("gone@1.0.0", "ISC")])
            .unwrap_err()
            .to_string();
        assert!(err.contains("gone@1.0.0"), "{err}");
    }

    #[test]
    fn exception_files_need_a_reason_an_spdx_id_and_a_pinned_version() {
        let good = "[[exception]]\npackage = \"a@1.0.0\"\nlicense = \"MPL-2.0\"\nreason = \"runtime dependency of the editor\"\n";
        assert_eq!(parse_exceptions(good).unwrap().len(), 1);
        assert!(parse_exceptions("").unwrap().is_empty());
        for bad in [
            "[[exception]]\npackage = \"a@1.0.0\"\nlicense = \"MPL-2.0\"\nreason = \" \"\n",
            "[[exception]]\npackage = \"a\"\nlicense = \"MPL-2.0\"\nreason = \"why\"\n",
            "[[exception]]\npackage = \"a@1.0.0\"\nlicense = \"Public Domain\"\nreason = \"why\"\n",
            "[[exception]]\npackage = \"a@1.0.0\"\nreason = \"why\"\n",
        ] {
            assert!(parse_exceptions(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_policy_value_that_differs_names_the_key_the_effective_and_the_expected_value() {
        check_policy("trustPolicy", "no-downgrade\n", "no-downgrade").unwrap();
        let err = check_policy("minimumReleaseAge", "undefined\n", "1440")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("minimumReleaseAge")
                && err.contains("`undefined`")
                && err.contains("`1440`"),
            "{err}"
        );
        assert!(check_policy("strictDepBuilds", "false", "true").is_err());
    }

    #[test]
    fn audit_signature_and_unused_reports_become_findings() {
        let audit = r#"{"advisories":{"1":{"github_advisory_id":"GHSA-35jh-r3h4-6jhm","module_name":"lodash"}}}"#;
        let findings = audit_findings(audit);
        assert!(
            expect_rejected(
                "web-audit",
                false,
                &findings,
                &[("vulnerability", "GHSA-35jh-r3h4-6jhm")]
            )
            .is_ok()
        );
        assert!(audit_findings("not json").is_empty());
        assert!(audit_findings("{\"advisories\":{}}").is_empty());

        let signatures =
            r#"{"audited":2,"invalid":[{"name":"left-pad","version":"1.3.0"}],"missing":[]}"#;
        assert!(
            expect_rejected(
                "web-signatures",
                false,
                &signature_findings(signatures),
                &[("invalid-signature", "left-pad")]
            )
            .is_ok()
        );

        let unused = r#"{"issues":[{"file":"package.json","dependencies":[{"name":"left-pad"}],"devDependencies":[],"unlisted":[{"name":"other"}]}]}"#;
        let findings = unused_findings(unused);
        assert!(
            expect_rejected(
                "web-unused",
                false,
                &findings,
                &[("unused-dependency", "left-pad")]
            )
            .is_ok()
        );
        assert!(
            expect_rejected(
                "web-unused",
                false,
                &findings,
                &[("unused-dependency", "other")]
            )
            .is_err()
        );
    }

    #[test]
    fn an_accepted_web_fixture_fails_the_step_and_names_the_fixture_and_code() {
        let err = expect_rejected(
            "web-unused",
            true,
            &[],
            &[("unused-dependency", "left-pad")],
        )
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("web-unused") && err.contains("unused-dependency"),
            "{err}"
        );
        let err = expect_rejected("web-audit", false, &[], &[("vulnerability", "GHSA-x")])
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("web-audit") && err.contains("vulnerability"),
            "{err}"
        );
    }
}
