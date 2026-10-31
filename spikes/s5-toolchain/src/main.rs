//! Spike S5: run checks for several repositories in parallel, each in its own
//! environment with the toolchain versions that repository declares, without
//! a version manager and without touching the host's global toolchains.
//!
//! Usage: s5-toolchain <plan.json> <round-name>

mod detect;
mod golang;
mod node;

use std::fs::{self, File};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use anyhow::{Context, Result, bail};

#[derive(Debug, Default, serde::Serialize)]
pub struct Provision {
    pub downloaded_bytes: u64,
    pub took_ms: u128,
    pub waited_ms: u128,
    pub verified: String,
}

impl Provision {
    fn cached(waited_ms: u128) -> Self {
        Provision {
            verified: "cache hit".into(),
            waited_ms,
            ..Default::default()
        }
    }
}

#[derive(serde::Deserialize)]
struct Plan {
    cache_dir: PathBuf,
    work_dir: PathBuf,
    memory_max: String,
    cpu_quota: String,
    tasks_max: String,
    envs: Vec<EnvSpec>,
}

#[derive(serde::Deserialize, Clone)]
struct EnvSpec {
    name: String,
    repo: String,
    check: String,
}

#[derive(serde::Serialize)]
struct EnvReport {
    name: String,
    commit: String,
    detected: detect::Detection,
    drift: Vec<String>,
    go: Option<(String, Provision)>,
    node: Option<(String, Provision)>,
    package_manager: Option<(String, Provision)>,
    probes: Vec<(String, String)>,
    check: String,
    check_ok: bool,
    check_ms: u128,
    log: PathBuf,
}

struct Indices {
    node: Vec<node::Release>,
    go: Vec<golang::Release>,
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("detect") {
        for repo in &args[2..] {
            let d = detect::detect(Path::new(repo));
            let first =
                |l: &[detect::Declared]| l.first().map(|d| format!("{} ({})", d.spec, d.source));
            println!(
                "{repo}\tgo={:?}\tnode={:?}\tpm={:?}\tdrift={:?}",
                first(&d.go),
                first(&d.node),
                d.package_manager,
                d.drift()
            );
        }
        return Ok(());
    }
    let [_, plan_path, round] = args.as_slice() else {
        bail!("usage: s5-toolchain <plan.json> <round-name> | s5-toolchain detect <repo>...");
    };
    let plan: Plan = serde_json::from_str(&fs::read_to_string(plan_path)?)?;
    let cache = absolute(&plan.cache_dir)?;
    let work = absolute(&plan.work_dir)?.join(round);
    fs::create_dir_all(&cache)?;
    let _ = fs::remove_dir_all(&work);
    fs::create_dir_all(&work)?;

    let global_before = global_versions();
    let started = Instant::now();
    let indices = Indices {
        node: node::index()?,
        go: golang::index()?,
    };

    let reports: Vec<Result<EnvReport>> = std::thread::scope(|s| {
        let handles: Vec<_> = plan
            .envs
            .iter()
            .map(|spec| s.spawn(|| run_env(&plan, spec, &cache, &work, &indices)))
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().expect("env thread panicked"))
            .collect()
    });

    let global_after = global_versions();
    let mut ok = global_before == global_after;
    let mut out = Vec::new();
    for (spec, r) in plan.envs.iter().zip(reports) {
        match r {
            Ok(r) => {
                ok &= r.check_ok;
                out.push(serde_json::to_value(&r)?);
            }
            Err(e) => {
                ok = false;
                out.push(serde_json::json!({ "name": spec.name, "error": format!("{e:#}") }));
            }
        }
    }
    let report = serde_json::json!({
        "round": round,
        "wall_ms": started.elapsed().as_millis(),
        "global_before": global_before,
        "global_after": global_after,
        "global_unchanged": global_before == global_after,
        "envs": out,
    });
    let path = work.join("report.json");
    fs::write(&path, serde_json::to_string_pretty(&report)?)?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    if !ok {
        bail!("round {round} failed, see {}", path.display());
    }
    Ok(())
}

fn run_env(
    plan: &Plan,
    spec: &EnvSpec,
    cache: &Path,
    work: &Path,
    idx: &Indices,
) -> Result<EnvReport> {
    let root = work.join(&spec.name);
    let tree = root.join("tree");
    let repo = expand_home(&spec.repo);
    let status = Command::new("git")
        .args(["clone", "--quiet", "--no-hardlinks"])
        .arg(&repo)
        .arg(&tree)
        .status()?;
    if !status.success() {
        bail!("clone {} failed", repo.display());
    }
    let commit = capture(
        Command::new("git")
            .args(["rev-parse", "--short", "HEAD"])
            .current_dir(&tree),
    )?;

    let detected = detect::detect(&tree);
    let drift = detected.drift();
    let home = std::env::var("HOME")?;
    let gomodcache = cache.join("go/mod");
    let tmp = root.join("tmp");
    let deny = root.join("deny");
    fs::create_dir_all(&tmp)?;
    fs::create_dir_all(&deny)?;

    let mut path: Vec<String> = Vec::new();
    let mut vars: Vec<(String, String)> = vec![
        ("HOME".into(), home),
        ("USER".into(), std::env::var("USER").unwrap_or_default()),
        ("LANG".into(), "C.UTF-8".into()),
        ("TMPDIR".into(), tmp.display().to_string()),
        (
            "XDG_CACHE_HOME".into(),
            cache.join("xdg/cache").display().to_string(),
        ),
        (
            "XDG_DATA_HOME".into(),
            cache.join("xdg/data").display().to_string(),
        ),
        (
            "XDG_STATE_HOME".into(),
            cache.join("xdg/state").display().to_string(),
        ),
    ];

    let go_version = match detected.go() {
        Some(d) => {
            let v = golang::resolve(&d.spec, &idx.go)?;
            vars.extend([
                ("GOTOOLCHAIN".into(), format!("go{v}")),
                ("GOMODCACHE".into(), gomodcache.display().to_string()),
                ("GOPATH".into(), cache.join("go/path").display().to_string()),
                (
                    "GOCACHE".into(),
                    cache.join("go/build").display().to_string(),
                ),
                ("GOFLAGS".into(), "-modcacherw".into()),
                ("GOENV".into(), "off".into()),
            ]);
            Some(v)
        }
        None => {
            write_deny(&deny, "go")?;
            None
        }
    };

    let node_version = match detected.node() {
        Some(d) => {
            let v = node::resolve(&d.spec, &idx.node)?;
            path.push(node::dir(cache, &v).join("bin").display().to_string());
            vars.extend([
                (
                    "COREPACK_HOME".into(),
                    cache.join("corepack").display().to_string(),
                ),
                ("COREPACK_ENABLE_DOWNLOAD_PROMPT".into(), "0".into()),
                (
                    "npm_config_cache".into(),
                    cache.join("npm").display().to_string(),
                ),
                (
                    "npm_config_store_dir".into(),
                    cache.join("pnpm-store").display().to_string(),
                ),
                ("npm_config_update_notifier".into(), "false".into()),
            ]);
            Some(v)
        }
        None => {
            for tool in ["node", "npm", "npx", "pnpm", "corepack"] {
                write_deny(&deny, tool)?;
            }
            None
        }
    };
    path.push(deny.display().to_string());
    path.extend(["/usr/local/bin", "/usr/bin", "/bin"].map(String::from));
    vars.push(("PATH".into(), path.join(":")));

    let go = match &go_version {
        Some(v) => Some((v.clone(), golang::provision(cache, &gomodcache, v, &vars)?)),
        None => None,
    };
    let node = match &node_version {
        Some(v) => {
            let p = node::provision(cache, v)?;
            enable_corepack(cache, v, &vars)?;
            Some((v.clone(), p))
        }
        None => None,
    };
    let package_manager = match (&detected.package_manager, &node_version) {
        (Some((name, v)), Some(_)) if name == "pnpm" => {
            Some((format!("pnpm@{v}"), provision_pnpm(cache, &tree, v, &vars)?))
        }
        _ => None,
    };

    let mut probes = Vec::new();
    let mut probe = |label: &str, cmd: &str| -> Result<()> {
        let out = run_limited(
            plan,
            &format!("{}-probe", spec.name),
            &tree,
            &vars,
            cmd,
            None,
        )?;
        probes.push((label.to_string(), out.1.trim().to_string()));
        Ok(())
    };
    if go_version.is_some() {
        probe("go", "go version")?;
    }
    if node_version.is_some() {
        probe("node", "node --version")?;
        match &package_manager {
            Some(_) => probe("pnpm", "pnpm --version")?,
            None => probe("npm", "npm --version")?,
        }
    } else {
        probe("node (not declared)", "node --version 2>&1 || true")?;
    }

    let log = root.join("check.log");
    let started = Instant::now();
    let (check_ok, _) = run_limited(plan, &spec.name, &tree, &vars, &spec.check, Some(&log))?;
    let check_ms = started.elapsed().as_millis();

    Ok(EnvReport {
        name: spec.name.clone(),
        commit,
        detected,
        drift,
        go,
        node,
        package_manager,
        probes,
        check: spec.check.clone(),
        check_ok,
        check_ms,
        log,
    })
}

/// Node 22 and 24 still ship Corepack; its shims go next to `node` in the
/// cached toolchain, so `pnpm` follows each repo's `packageManager`.
fn enable_corepack(cache: &Path, version: &str, vars: &[(String, String)]) -> Result<()> {
    let bin = node::dir(cache, version).join("bin");
    if bin.join("pnpm").exists() || !bin.join("corepack").exists() {
        return Ok(());
    }
    let lock = File::create(cache.join(format!("toolchains/node/.{version}.lock")))?;
    lock.lock()?;
    if bin.join("pnpm").exists() {
        return Ok(());
    }
    let status = Command::new(bin.join("corepack"))
        .args(["enable", "--install-directory"])
        .arg(&bin)
        .arg("pnpm")
        .env_clear()
        .envs(vars.iter().cloned())
        .status()?;
    if !status.success() {
        bail!("corepack enable failed for node {version}");
    }
    Ok(())
}

fn provision_pnpm(
    cache: &Path,
    tree: &Path,
    version: &str,
    vars: &[(String, String)],
) -> Result<Provision> {
    let locks = cache.join("locks");
    fs::create_dir_all(&locks)?;
    let lock = File::create(locks.join(format!("pnpm{version}.lock")))?;
    let waited = Instant::now();
    lock.lock()?;
    let waited_ms = waited.elapsed().as_millis();
    let installed = cache.join("corepack/v1/pnpm").join(version);
    if installed.exists() {
        return Ok(Provision::cached(waited_ms));
    }
    let started = Instant::now();
    let out = Command::new("pnpm")
        .arg("--version")
        .current_dir(tree)
        .env_clear()
        .envs(vars.iter().cloned())
        .output()?;
    if !out.status.success() {
        bail!("pnpm {version}: {}", String::from_utf8_lossy(&out.stderr));
    }
    Ok(Provision {
        downloaded_bytes: dir_size(&installed),
        took_ms: started.elapsed().as_millis(),
        waited_ms,
        verified: "corepack (npm registry signature)".into(),
    })
}

static UNIT_SEQ: AtomicU64 = AtomicU64::new(0);

/// Runs `cmd` in its own user cgroup scope with only `vars` as environment.
fn run_limited(
    plan: &Plan,
    name: &str,
    cwd: &Path,
    vars: &[(String, String)],
    cmd: &str,
    log: Option<&Path>,
) -> Result<(bool, String)> {
    let mut c = Command::new("systemd-run");
    c.args(["--user", "--scope", "--quiet", "--collect"])
        .arg(format!(
            "--unit=saka-s5-{name}-{}-{}",
            std::process::id(),
            UNIT_SEQ.fetch_add(1, Ordering::Relaxed)
        ))
        .args(["-p", &format!("MemoryMax={}", plan.memory_max)])
        .args(["-p", &format!("CPUQuota={}", plan.cpu_quota)])
        .args(["-p", &format!("TasksMax={}", plan.tasks_max)])
        .args(["--", "env", "-i"])
        .args(vars.iter().map(|(k, v)| format!("{k}={v}")))
        .args(["sh", "-c", cmd])
        .current_dir(cwd)
        .stdin(Stdio::null());
    match log {
        Some(path) => {
            let f = File::create(path)?;
            c.stdout(f.try_clone()?).stderr(f);
            Ok((c.status()?.success(), String::new()))
        }
        None => {
            let out = c.output()?;
            let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
            text.push_str(&String::from_utf8_lossy(&out.stderr));
            Ok((out.status.success(), text))
        }
    }
}

fn write_deny(dir: &Path, tool: &str) -> Result<()> {
    let path = dir.join(tool);
    fs::write(
        &path,
        format!("#!/bin/sh\necho '{tool}: not declared by this repository' >&2\nexit 127\n"),
    )?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755))?;
    Ok(())
}

/// What the host's own shell sees, outside any environment.
fn global_versions() -> Vec<(String, String)> {
    [
        "go version",
        "node --version",
        "pnpm --version",
        "rustc --version",
    ]
    .iter()
    .map(|cmd| {
        let out = Command::new("zsh")
            .args(["-lc", cmd])
            .current_dir("/tmp")
            .output();
        let text = out
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default();
        (cmd.to_string(), text)
    })
    .collect()
}

fn capture(cmd: &mut Command) -> Result<String> {
    let out = cmd.output()?;
    Ok(String::from_utf8(out.stdout)?.trim().to_string())
}

fn dir_size(path: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(path) else {
        return 0;
    };
    entries
        .flatten()
        .map(|e| match e.file_type() {
            Ok(t) if t.is_dir() => dir_size(&e.path()),
            Ok(_) => e.metadata().map(|m| m.len()).unwrap_or(0),
            Err(_) => 0,
        })
        .sum()
}

fn expand_home(p: &str) -> PathBuf {
    match p.strip_prefix("~/") {
        Some(rest) => PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(rest),
        None => PathBuf::from(p),
    }
}

fn absolute(p: &Path) -> Result<PathBuf> {
    let cwd = std::env::current_dir().context("cwd")?;
    Ok(if p.is_absolute() {
        p.to_path_buf()
    } else {
        cwd.join(p)
    })
}
