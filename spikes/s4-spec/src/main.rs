//! Spike S4: how many existing specs fit the task model as written, and which
//! markdown parser to use on the server.
//!
//! Usage:
//!   s4-spec specs <vault> <project>...   spec model + lint per project
//!   s4-spec parsers <vault>               pulldown-cmark vs comrak on every note

mod compare;
mod spec;
mod vault;

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::Path;

use anyhow::{Result, bail};

use spec::{Spec, canonical};
use vault::{Resolved, Vault};

#[derive(serde::Serialize)]
struct Verdict {
    path: String,
    acs: usize,
    steps: usize,
    strict: bool,
    lenient: bool,
    model: Vec<String>,
    dor: Vec<String>,
    spec: Spec,
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("specs") if args.len() >= 4 => specs(Path::new(&args[2]), &args[3..]),
        Some("parsers") if args.len() == 3 => parsers(Path::new(&args[2])),
        _ => bail!("usage: s4-spec specs <vault> <project>... | s4-spec parsers <vault>"),
    }
}

fn specs(root: &Path, projects: &[String]) -> Result<()> {
    let vault = Vault::load(root);
    let out_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("out");
    fs::create_dir_all(&out_dir)?;
    println!(
        "vault: {} files, {} notes, symlinks skipped: {:?}",
        vault.files.len(),
        vault.markdown().count(),
        vault.symlinks
    );

    let mut all_ids: BTreeMap<String, usize> = BTreeMap::new();
    for project in projects {
        let prefix = format!("1-projects/{project}/specs/");
        let paths: Vec<&String> = vault
            .markdown()
            .filter(|f| f.starts_with(&prefix) && !f.ends_with("/README.md"))
            .collect();
        let mut verdicts = Vec::new();
        for path in paths {
            let text = fs::read_to_string(root.join(path))?;
            let s = spec::extract(&text);
            for ac in &s.acs {
                *all_ids.entry(id_shape(&ac.id)).or_default() += 1;
            }
            verdicts.push(lint(&vault, path, s));
        }
        report(project, &verdicts);
        fs::write(
            out_dir.join(format!("{project}.json")),
            serde_json::to_string_pretty(&verdicts)?,
        )?;
    }
    println!("\nAC ID shapes across all projects: {all_ids:?}");
    Ok(())
}

fn id_shape(id: &str) -> String {
    id.chars()
        .map(|c| if c.is_ascii_digit() { 'N' } else { c })
        .collect()
}

fn lint(vault: &Vault, path: &str, s: Spec) -> Verdict {
    let mut model = Vec::new();
    let mut strict_only = Vec::new();
    let mut dor = Vec::new();

    if s.acs.is_empty() {
        model.push("no-ac".to_string());
    }
    let mut seen = HashSet::new();
    let dups: Vec<&str> = s
        .acs
        .iter()
        .filter(|a| !seen.insert(a.id.as_str()))
        .map(|a| a.id.as_str())
        .collect();
    if !dups.is_empty() {
        model.push(format!("ac-duplicate {dups:?}"));
    }
    let odd: Vec<&str> = s
        .acs
        .iter()
        .filter(|a| !canonical(&a.id))
        .map(|a| a.id.as_str())
        .collect();
    if !odd.is_empty() {
        strict_only.push(format!("ac-id-noncanonical {:?}", &odd[..odd.len().min(4)]));
    }
    let forms: HashSet<&str> = s
        .acs
        .iter()
        .map(|a| a.form)
        .filter(|f| *f != "task")
        .collect();
    if !forms.is_empty() {
        strict_only.push(format!("ac-form {forms:?}"));
    }

    if s.acs_outside_section {
        strict_only.push("ac-outside-section".into());
    }
    if s.step_form == Some("list") {
        strict_only.push("steps-as-list".into());
    }
    // An order section without a table or list is prose ("Single step, ..."): one task.
    if s.has_order_section && s.steps.is_empty() && !s.step_columns.is_empty() {
        model.push(format!("steps-unparsed columns={:?}", s.step_columns));
    }
    let defined: HashSet<&str> = s.acs.iter().map(|a| a.id.as_str()).collect();
    let numeric: Vec<Option<u32>> = s
        .steps
        .iter()
        .map(|st| st.id.trim_matches('*').trim().parse().ok())
        .collect();
    if numeric
        .iter()
        .enumerate()
        .any(|(i, n)| *n != Some(i as u32 + 1))
    {
        let ids: Vec<&str> = s.steps.iter().map(|st| st.id.as_str()).collect();
        strict_only.push(format!("step-ids {:?}", &ids[..ids.len().min(6)]));
    }
    let bare: Vec<&str> = s
        .steps
        .iter()
        .filter(|st| st.ac_refs.is_empty())
        .map(|st| st.id.as_str())
        .collect();
    if !bare.is_empty() {
        strict_only.push(format!("step-without-ac {bare:?}"));
    }
    let missing: Vec<&String> = s
        .steps
        .iter()
        .flat_map(|st| &st.ac_refs)
        .filter(|r| !defined.contains(r.as_str()))
        .collect();
    if !missing.is_empty() {
        model.push(format!("step-ref-missing {missing:?}"));
    }
    if !s.steps.is_empty() {
        let used: HashSet<&str> = s
            .steps
            .iter()
            .flat_map(|st| st.ac_refs.iter().map(String::as_str))
            .collect();
        let unassigned = s
            .acs
            .iter()
            .filter(|a| !used.contains(a.id.as_str()))
            .count();
        if unassigned > 0 && !bare.is_empty() {
            // Steps without explicit ACs may cover the rest; only informational.
        } else if unassigned > 0 {
            dor.push(format!("ac-unassigned {unassigned}"));
        }
    }

    for (target, line) in &s.links {
        match vault.resolve(target, path) {
            Resolved::Missing => dor.push(format!("ref-missing L{line} {target}")),
            Resolved::Ambiguous(_) => {}
            Resolved::One(_) => {}
        }
    }
    match &s.approach {
        None => dor.push("decision-section-missing".into()),
        Some(a) => {
            let lower = a.to_lowercase();
            let placeholder =
                a.contains("{owner-selected") || a.contains("{why ") || a.contains("YYYY-MM-DD");
            if placeholder || lower.contains("tbd") || lower.contains("pending owner") {
                dor.push("decision-open".into());
            }
        }
    }
    if s.dor_total > s.dor_checked {
        dor.push(format!(
            "dor-unchecked {}/{}",
            s.dor_total - s.dor_checked,
            s.dor_total
        ));
    }
    if s.verification_cmds == 0 {
        dor.push("no-verification-cmd".into());
    }

    let lenient = model.is_empty();
    let strict = lenient && strict_only.is_empty();
    model.extend(strict_only);
    Verdict {
        path: path.to_string(),
        acs: s.acs.len(),
        steps: s.steps.len(),
        strict,
        lenient,
        model,
        dor,
        spec: s,
    }
}

fn report(project: &str, v: &[Verdict]) {
    let n = v.len();
    let pct = |k: usize| {
        if n == 0 {
            0.0
        } else {
            100.0 * k as f64 / n as f64
        }
    };
    let strict = v.iter().filter(|x| x.strict).count();
    let lenient = v.iter().filter(|x| x.lenient).count();
    let multi = v.iter().filter(|x| x.steps > 0).count();
    let acs: usize = v.iter().map(|x| x.acs).sum();
    let clean_dor = v.iter().filter(|x| x.dor.is_empty()).count();
    println!("\n== {project}: {n} specs, {acs} ACs, {multi} multi-step");
    println!(
        "  pass as written: strict {strict} ({:.0}%), lenient {lenient} ({:.0}%)",
        pct(strict),
        pct(lenient)
    );
    println!("  DoR lint clean: {clean_dor} ({:.0}%)", pct(clean_dor));
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for x in v {
        let kinds: HashSet<String> = x
            .model
            .iter()
            .chain(&x.dor)
            .map(|i| i.split_whitespace().next().unwrap_or("").to_string())
            .collect();
        for k in kinds {
            *counts.entry(k).or_default() += 1;
        }
    }
    let mut counts: Vec<_> = counts.into_iter().collect();
    counts.sort_by_key(|a| std::cmp::Reverse(a.1));
    println!("  issues (specs affected): {counts:?}");
    let mut per_step: Vec<usize> = v
        .iter()
        .flat_map(|x| x.spec.steps.iter().map(|s| s.ac_refs.len()))
        .collect();
    per_step.sort();
    if !per_step.is_empty() {
        let q = |p: f64| per_step[((per_step.len() - 1) as f64 * p) as usize];
        println!(
            "  ACs per step: n={} median={} p90={} max={}",
            per_step.len(),
            q(0.5),
            q(0.9),
            q(1.0)
        );
    }
}

fn parsers(root: &Path) -> Result<()> {
    let vault = Vault::load(root);
    let mut docs = Vec::new();
    let mut not_utf8 = Vec::new();
    for path in vault.markdown() {
        match fs::read_to_string(root.join(path)) {
            Ok(t) => docs.push((path.clone(), t)),
            Err(_) => not_utf8.push(path.clone()),
        }
    }
    let c = compare::run(&docs);
    println!(
        "notes: {} ({} MB), not UTF-8: {not_utf8:?}",
        c.files,
        c.bytes >> 20
    );
    println!(
        "same headings: {} ({:.2}%)",
        c.same_headings,
        100.0 * c.same_headings as f64 / c.files as f64
    );
    println!(
        "same wiki-links: {} ({:.2}%)",
        c.same_links,
        100.0 * c.same_links as f64 / c.files as f64
    );
    println!(
        "time: pulldown-cmark {:?}, comrak {:?}",
        c.pulldown_time, c.comrak_time
    );
    for d in &c.diffs {
        println!("  {d}");
    }
    Ok(())
}
