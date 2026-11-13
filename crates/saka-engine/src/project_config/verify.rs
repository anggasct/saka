use std::collections::BTreeMap;

use saka_core::environment::{
    MAX_VERIFY_COMMANDS, READY_INTERVAL_RANGE, READY_TIMEOUT_RANGE, VERIFY_TIMEOUT_MAX_SECS,
    VerifySpec, command_ok, within,
};
use saka_core::hook::slug_ok;
use toml::Spanned;
use toml::de::{DeTable, DeValue};

use super::{Collector, get};

const KEYS: &[&str] = &[
    "ready",
    "ready_timeout_secs",
    "ready_interval_secs",
    "commands",
    "timeout_secs",
];

fn seconds(
    c: &mut Collector<'_>,
    table: &DeTable<'_>,
    key: &str,
    field: &str,
    allowed: (u64, u64),
) -> Option<u64> {
    let v = get(table, key)?;
    let parsed = v
        .get_ref()
        .as_integer()
        .and_then(|n| n.as_str().parse::<u64>().ok());
    match parsed {
        Some(n) if within(n, allowed) => Some(n),
        Some(_) => {
            c.push("verify_timeout_invalid", field, Some(v.span().start));
            None
        }
        None => {
            c.push("field_invalid", field, Some(v.span().start));
            None
        }
    }
}

fn commands(c: &mut Collector<'_>, field: &str, value: &Spanned<DeValue<'_>>) -> Vec<String> {
    let at = value.span().start;
    let Some(items) = value.get_ref().as_array() else {
        c.push("field_invalid", field, Some(at));
        return Vec::new();
    };
    if items.len() > MAX_VERIFY_COMMANDS {
        c.push("verify_too_many", field, Some(at));
        return Vec::new();
    }
    let mut out = Vec::new();
    for (index, item) in items.iter().enumerate() {
        match item.get_ref().as_str() {
            Some(text) if command_ok(text) => out.push(text.to_string()),
            Some(_) => c.push(
                "verify_command_invalid",
                &format!("{field}[{index}]"),
                Some(item.span().start),
            ),
            None => c.push(
                "field_invalid",
                &format!("{field}[{index}]"),
                Some(item.span().start),
            ),
        }
    }
    out
}

fn environment(c: &mut Collector<'_>, name: &str, value: &Spanned<DeValue<'_>>) -> VerifySpec {
    let prefix = format!("verify.{name}");
    let mut spec = VerifySpec::default();
    let Some(table) = value.get_ref().as_table() else {
        c.push("field_invalid", &prefix, Some(value.span().start));
        return spec;
    };
    c.unknown(table, &format!("{prefix}."), KEYS);
    if let Some(v) = get(table, "ready") {
        match v.get_ref().as_str() {
            Some(text) if command_ok(text) => spec.ready = Some(text.to_string()),
            _ => c.push(
                "field_invalid",
                &format!("{prefix}.ready"),
                Some(v.span().start),
            ),
        }
    }
    let field = |key: &str| format!("{prefix}.{key}");
    if let Some(n) = seconds(
        c,
        table,
        "ready_timeout_secs",
        &field("ready_timeout_secs"),
        READY_TIMEOUT_RANGE,
    ) {
        spec.ready_timeout_secs = n;
    }
    if let Some(n) = seconds(
        c,
        table,
        "ready_interval_secs",
        &field("ready_interval_secs"),
        READY_INTERVAL_RANGE,
    ) {
        spec.ready_interval_secs = n;
    }
    if let Some(n) = seconds(
        c,
        table,
        "timeout_secs",
        &field("timeout_secs"),
        (1, VERIFY_TIMEOUT_MAX_SECS),
    ) {
        spec.timeout_secs = n;
    }
    if let Some(v) = get(table, "commands") {
        spec.commands = commands(c, &field("commands"), v);
    }
    spec
}

pub fn parse(c: &mut Collector<'_>, doc: &DeTable<'_>) -> BTreeMap<String, VerifySpec> {
    let mut out = BTreeMap::new();
    let Some(section) = get(doc, "verify") else {
        return out;
    };
    let Some(table) = section.get_ref().as_table() else {
        c.push("field_invalid", "verify", Some(section.span().start));
        return out;
    };
    for (key, value) in table.iter() {
        let name = key.get_ref().as_ref();
        if !slug_ok(name) {
            c.push(
                "field_invalid",
                &format!("verify.{name}"),
                Some(key.span().start),
            );
            continue;
        }
        out.insert(name.to_string(), environment(c, name, value));
    }
    out
}
