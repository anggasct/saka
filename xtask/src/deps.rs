use std::collections::{BTreeSet, HashMap};
use std::fmt;

use anyhow::{Result, bail};
use serde_json::Value;

use crate::metadata;

const ADAPTER_DEPS: &[&str] = &["saka-core", "saka-ports"];

const ALLOWED: &[(&str, &[&str])] = &[
    ("saka-core", &[]),
    ("saka-spec", &["saka-core"]),
    ("saka-wasm", &["saka-core", "saka-spec"]),
    ("saka-ports", &["saka-core"]),
    ("saka-store", &["saka-core"]),
    ("saka-docs", ADAPTER_DEPS),
    ("saka-agents", ADAPTER_DEPS),
    ("saka-env", ADAPTER_DEPS),
    ("saka-scm", ADAPTER_DEPS),
    ("saka-connectors", ADAPTER_DEPS),
    ("saka-notify", ADAPTER_DEPS),
    ("saka-intake", ADAPTER_DEPS),
    (
        "saka-engine",
        &["saka-core", "saka-spec", "saka-ports", "saka-store"],
    ),
    (
        "saka-api",
        &["saka-core", "saka-spec", "saka-ports", "saka-engine"],
    ),
    ("saka-mcp", &["saka-core", "saka-ports", "saka-engine"]),
    (
        "saka",
        &[
            "saka-core",
            "saka-spec",
            "saka-ports",
            "saka-store",
            "saka-engine",
            "saka-docs",
            "saka-agents",
            "saka-env",
            "saka-scm",
            "saka-connectors",
            "saka-notify",
            "saka-intake",
            "saka-api",
            "saka-mcp",
        ],
    ),
    (
        "saka-testkit",
        &[
            "saka-core",
            "saka-spec",
            "saka-ports",
            "saka-store",
            "saka-engine",
        ],
    ),
    ("xtask", &[]),
];

const TESTKIT: &str = "saka-testkit";

const PURE: &[&str] = &["saka-core", "saka-spec"];

const FORBIDDEN_IN_PURE: &[&str] = &[
    "tokio", "rusqlite", "reqwest", "axum", "hyper", "mio", "socket2", "rand",
];

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Violation {
    NotInMap {
        krate: String,
    },
    Edge {
        from: String,
        to: String,
        dev: bool,
    },
    Forbidden {
        krate: String,
        dependency: String,
        path: Vec<String>,
    },
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Violation::NotInMap { krate } => {
                write!(
                    f,
                    "{krate}: not in the crate map; adding a crate needs a decision record"
                )
            }
            Violation::Edge { from, to, dev } => {
                let kind = if *dev { "dev-dependency" } else { "dependency" };
                write!(f, "{from} -> {to}: {kind} not allowed by the crate map")
            }
            Violation::Forbidden {
                krate,
                dependency,
                path,
            } => write!(
                f,
                "{krate} must not depend on {dependency} (via {})",
                path.join(" -> ")
            ),
        }
    }
}

pub fn run() -> Result<()> {
    let meta = metadata::load()?;
    let violations = check(&meta);
    if violations.is_empty() {
        println!("deps: ok");
        return Ok(());
    }
    for v in &violations {
        eprintln!("deps: {v}");
    }
    bail!("{} dependency violation(s)", violations.len())
}

struct Graph<'a> {
    names: HashMap<&'a str, &'a str>,
    edges: HashMap<&'a str, Vec<(&'a str, bool, bool)>>,
}

impl<'a> Graph<'a> {
    fn new(meta: &'a Value) -> Graph<'a> {
        let mut names = HashMap::new();
        for pkg in meta["packages"].as_array().into_iter().flatten() {
            if let (Some(id), Some(name)) = (pkg["id"].as_str(), pkg["name"].as_str()) {
                names.insert(id, name);
            }
        }
        let mut edges: HashMap<&str, Vec<(&str, bool, bool)>> = HashMap::new();
        for node in meta["resolve"]["nodes"].as_array().into_iter().flatten() {
            let Some(id) = node["id"].as_str() else {
                continue;
            };
            for dep in node["deps"].as_array().into_iter().flatten() {
                let Some(pkg) = dep["pkg"].as_str() else {
                    continue;
                };
                let kinds: Vec<Option<&str>> = dep["dep_kinds"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|k| k["kind"].as_str())
                    .collect();
                let runtime = kinds.iter().any(|k| matches!(k, None | Some("build")));
                let dev = kinds.contains(&Some("dev"));
                edges.entry(id).or_default().push((pkg, runtime, dev));
            }
        }
        Graph { names, edges }
    }

    fn name(&self, id: &'a str) -> &'a str {
        self.names.get(id).copied().unwrap_or(id)
    }

    fn runtime_path(&self, start: &'a str, target: &str) -> Option<Vec<String>> {
        let mut seen = BTreeSet::from([start]);
        let mut queue = std::collections::VecDeque::from([vec![start]]);
        while let Some(path) = queue.pop_front() {
            let last = path[path.len() - 1];
            for &(dep, runtime, _) in self.edges.get(last).into_iter().flatten() {
                if !runtime || !seen.insert(dep) {
                    continue;
                }
                let mut next = path.clone();
                next.push(dep);
                if self.name(dep) == target {
                    return Some(next.iter().map(|id| self.name(id).to_string()).collect());
                }
                queue.push_back(next);
            }
        }
        None
    }
}

fn allowed(krate: &str) -> Option<&'static [&'static str]> {
    ALLOWED
        .iter()
        .find(|(name, _)| *name == krate)
        .map(|(_, deps)| *deps)
}

pub fn check(meta: &Value) -> Vec<Violation> {
    let graph = Graph::new(meta);
    let product: BTreeSet<String> = metadata::product_packages(meta)
        .unwrap_or_default()
        .into_iter()
        .collect();
    let members: Vec<&str> = meta["workspace_members"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let mut violations = BTreeSet::new();

    for &id in &members {
        let from = graph.name(id);
        if !product.contains(from) {
            continue;
        }
        let Some(allow) = allowed(from) else {
            violations.insert(Violation::NotInMap {
                krate: from.to_string(),
            });
            continue;
        };
        for &(dep, runtime, dev) in graph.edges.get(id).into_iter().flatten() {
            if !members.contains(&dep) {
                continue;
            }
            let to = graph.name(dep);
            if runtime && !allow.contains(&to) {
                violations.insert(Violation::Edge {
                    from: from.to_string(),
                    to: to.to_string(),
                    dev: false,
                });
            }
            let dev_ok = allow.contains(&to)
                || (to == TESTKIT && from != TESTKIT)
                || allowed(TESTKIT).is_some_and(|t| t.contains(&to));
            if dev && !dev_ok {
                violations.insert(Violation::Edge {
                    from: from.to_string(),
                    to: to.to_string(),
                    dev: true,
                });
            }
        }
        if PURE.contains(&from) {
            for forbidden in FORBIDDEN_IN_PURE {
                if let Some(path) = graph.runtime_path(id, forbidden) {
                    violations.insert(Violation::Forbidden {
                        krate: from.to_string(),
                        dependency: (*forbidden).to_string(),
                        path,
                    });
                }
            }
        }
    }
    violations.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::{Violation, check};

    fn meta(members: &[&str], external: &[&str], edges: &[(&str, &str, Option<&str>)]) -> Value {
        let id = |n: &str| format!("{n} 0.1.0");
        let packages: Vec<Value> = members
            .iter()
            .map(|n| json!({ "id": id(n), "name": n, "manifest_path": format!("/w/crates/{n}/Cargo.toml") }))
            .chain(external.iter().map(|n| {
                json!({ "id": id(n), "name": n, "manifest_path": format!("/registry/{n}/Cargo.toml") })
            }))
            .collect();
        let mut nodes: Vec<Value> = Vec::new();
        for n in members.iter().chain(external) {
            let deps: Vec<Value> = edges
                .iter()
                .filter(|(from, _, _)| from == n)
                .map(|(_, to, kind)| json!({ "name": to, "pkg": id(to), "dep_kinds": [{ "kind": kind, "target": null }] }))
                .collect();
            nodes.push(json!({ "id": id(n), "deps": deps }));
        }
        json!({
            "workspace_root": "/w",
            "workspace_members": members.iter().map(|n| id(n)).collect::<Vec<_>>(),
            "packages": packages,
            "resolve": { "nodes": nodes },
        })
    }

    #[test]
    fn allowed_graph_passes() {
        let m = meta(
            &[
                "saka-core",
                "saka-spec",
                "saka-ports",
                "saka-store",
                "saka-engine",
                "saka-testkit",
            ],
            &["serde", "proptest", "rand"],
            &[
                ("saka-spec", "saka-core", None),
                ("saka-ports", "saka-core", None),
                ("saka-engine", "saka-store", None),
                ("saka-engine", "saka-testkit", Some("dev")),
                ("saka-core", "serde", None),
                ("saka-core", "proptest", Some("dev")),
                ("proptest", "rand", None),
            ],
        );
        assert_eq!(check(&m), vec![]);
    }

    #[test]
    fn forbidden_edge_is_named() {
        let m = meta(
            &["saka-core", "saka-store"],
            &[],
            &[("saka-core", "saka-store", None)],
        );
        let v = check(&m);
        assert_eq!(
            v,
            vec![Violation::Edge {
                from: "saka-core".into(),
                to: "saka-store".into(),
                dev: false
            }]
        );
        assert_eq!(
            v[0].to_string(),
            "saka-core -> saka-store: dependency not allowed by the crate map"
        );
    }

    #[test]
    fn adapters_do_not_depend_on_each_other() {
        let m = meta(
            &["saka-core", "saka-ports", "saka-scm", "saka-env"],
            &[],
            &[("saka-scm", "saka-env", None)],
        );
        assert_eq!(
            check(&m),
            vec![Violation::Edge {
                from: "saka-scm".into(),
                to: "saka-env".into(),
                dev: false
            }]
        );
    }

    #[test]
    fn crate_outside_the_map_is_reported() {
        let m = meta(&["saka-core", "saka-extra"], &[], &[]);
        assert_eq!(
            check(&m),
            vec![Violation::NotInMap {
                krate: "saka-extra".into()
            }]
        );
    }

    #[test]
    fn pure_crate_with_runtime_in_tree_is_reported_with_path() {
        for forbidden in ["tokio", "rusqlite", "reqwest", "axum", "rand"] {
            let m = meta(
                &["saka-core", "saka-spec"],
                &["helper", forbidden],
                &[
                    ("saka-spec", "saka-core", None),
                    ("saka-spec", "helper", None),
                    ("helper", forbidden, None),
                ],
            );
            assert_eq!(
                check(&m),
                vec![Violation::Forbidden {
                    krate: "saka-spec".into(),
                    dependency: forbidden.into(),
                    path: vec!["saka-spec".into(), "helper".into(), forbidden.into()],
                }],
                "{forbidden}"
            );
        }
    }

    #[test]
    fn dev_dependencies_of_pure_crates_are_not_checked_for_runtimes() {
        let m = meta(
            &["saka-core"],
            &["tokio"],
            &[("saka-core", "tokio", Some("dev"))],
        );
        assert_eq!(check(&m), vec![]);
    }
}
