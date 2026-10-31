//! Reads the toolchain versions a repository declares.
//!
//! Order per ecosystem: the ecosystem's own version file first, then CI
//! workflows, then the Dockerfile. Every source found is kept so drift between
//! them can be reported, but only the first one decides.

use std::fs;
use std::path::Path;

#[derive(Debug, Clone, serde::Serialize)]
pub struct Declared {
    pub spec: String,
    pub source: String,
}

#[derive(Debug, Default, serde::Serialize)]
pub struct Detection {
    pub go: Vec<Declared>,
    pub node: Vec<Declared>,
    pub package_manager: Option<(String, String)>,
}

impl Detection {
    pub fn go(&self) -> Option<&Declared> {
        self.go.first()
    }

    pub fn node(&self) -> Option<&Declared> {
        self.node.first()
    }

    /// Sources that disagree with the deciding one, e.g. go.mod 1.26 vs Dockerfile 1.24.
    pub fn drift(&self) -> Vec<String> {
        let mut out = Vec::new();
        for (name, list) in [("go", &self.go), ("node", &self.node)] {
            let Some(first) = list.first() else { continue };
            for other in &list[1..] {
                if major_minor(&other.spec) != major_minor(&first.spec) {
                    out.push(format!(
                        "{name}: {} says {} but {} says {}",
                        first.source, first.spec, other.source, other.spec
                    ));
                }
            }
        }
        out
    }
}

fn major_minor(spec: &str) -> String {
    let s = spec.trim_start_matches('v').trim_start_matches(">=");
    s.split('.')
        .take(2)
        .filter(|p| *p != "x")
        .collect::<Vec<_>>()
        .join(".")
}

pub fn detect(repo: &Path) -> Detection {
    let mut d = Detection::default();

    if let Ok(gomod) = fs::read_to_string(repo.join("go.mod")) {
        let line = |key: &str| {
            gomod
                .lines()
                .find_map(|l| l.trim().strip_prefix(key).map(|v| v.trim().to_string()))
        };
        if let Some(tc) = line("toolchain ") {
            let spec = tc.trim_start_matches("go").to_string();
            d.go.push(Declared {
                spec,
                source: "go.mod toolchain".into(),
            });
        }
        if let Some(v) = line("go ") {
            d.go.push(Declared {
                spec: v,
                source: "go.mod go".into(),
            });
        }
    }

    for file in [".nvmrc", ".node-version"] {
        if let Ok(s) = fs::read_to_string(repo.join(file)) {
            let spec = s.lines().next().unwrap_or("").trim().to_string();
            if !spec.is_empty() {
                d.node.push(Declared {
                    spec,
                    source: file.into(),
                });
            }
        }
    }

    if let Ok(s) = fs::read_to_string(repo.join("package.json"))
        && let Ok(pkg) = serde_json::from_str::<serde_json::Value>(&s)
    {
        let runtime = &pkg["devEngines"]["runtime"];
        if runtime["name"] == "node"
            && let Some(v) = runtime["version"].as_str()
        {
            d.node.push(Declared {
                spec: v.into(),
                source: "devEngines.runtime".into(),
            });
        }
        if let Some(v) = pkg["engines"]["node"].as_str() {
            d.node.push(Declared {
                spec: v.into(),
                source: "engines.node".into(),
            });
        }
        if let Some(pm) = pkg["packageManager"].as_str()
            && let Some((name, rest)) = pm.split_once('@')
        {
            let version = rest.split('+').next().unwrap_or(rest);
            d.package_manager = Some((name.into(), version.into()));
        }
    }

    for (key, list) in [("node-version:", &mut d.node), ("go-version:", &mut d.go)] {
        if let Some((spec, file)) = ci_value(repo, key) {
            list.push(Declared {
                spec,
                source: format!("CI {file}"),
            });
        }
    }

    if let Ok(s) = fs::read_to_string(repo.join("Dockerfile")) {
        for (image, list) in [("node:", &mut d.node), ("golang:", &mut d.go)] {
            if let Some(tag) = docker_tag(&s, image) {
                list.push(Declared {
                    spec: tag,
                    source: "Dockerfile".into(),
                });
            }
        }
    }

    d
}

/// First `key: value` in .github/workflows, skipping matrix expressions.
fn ci_value(repo: &Path, key: &str) -> Option<(String, String)> {
    let dir = repo.join(".github/workflows");
    let mut files: Vec<_> = fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .collect();
    files.sort();
    for path in files {
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        for line in text.lines() {
            let Some(v) = line.trim().strip_prefix(key) else {
                continue;
            };
            let v = v
                .split('#')
                .next()
                .unwrap_or("")
                .trim()
                .trim_matches(['"', '\'']);
            if !v.is_empty() && !v.contains("${{") {
                let name = path.file_name().unwrap().to_string_lossy().into_owned();
                return Some((v.to_string(), name));
            }
        }
    }
    None
}

/// `FROM node:22-alpine AS build` gives `22`.
fn docker_tag(dockerfile: &str, image: &str) -> Option<String> {
    dockerfile.lines().find_map(|l| {
        let mut words = l.split_whitespace();
        if !words.next()?.eq_ignore_ascii_case("FROM") {
            return None;
        }
        let img = words.find(|w| !w.starts_with("--"))?;
        let tag = img.strip_prefix(image)?;
        let version: String = tag
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .collect();
        (!version.is_empty()).then_some(version)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn docker_tags() {
        let df = "FROM --platform=$BUILDPLATFORM golang:1.24-alpine AS build\nFROM node:22-slim\n";
        assert_eq!(docker_tag(df, "golang:").as_deref(), Some("1.24"));
        assert_eq!(docker_tag(df, "node:").as_deref(), Some("22"));
    }

    #[test]
    fn drift_compares_minor() {
        let d = Detection {
            go: vec![
                Declared {
                    spec: "1.26.1".into(),
                    source: "go.mod go".into(),
                },
                Declared {
                    spec: "1.24".into(),
                    source: "Dockerfile".into(),
                },
            ],
            node: vec![
                Declared {
                    spec: "v24".into(),
                    source: ".nvmrc".into(),
                },
                Declared {
                    spec: "24.x".into(),
                    source: "CI ci.yml".into(),
                },
            ],
            package_manager: None,
        };
        assert_eq!(d.drift().len(), 1);
    }
}
