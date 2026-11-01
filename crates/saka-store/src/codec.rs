use std::str::FromStr;

use rusqlite::Row;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use uuid::Uuid;

use crate::error::{Result, StoreError};

pub(crate) fn uuid(row: &Row<'_>, col: &str) -> Result<Uuid> {
    let s: String = row.get(col)?;
    Uuid::parse_str(&s).map_err(|_| StoreError::Corrupt(format!("{col}: not a uuid: {s}")))
}

pub(crate) fn opt_uuid(row: &Row<'_>, col: &str) -> Result<Option<Uuid>> {
    let s: Option<String> = row.get(col)?;
    s.map(|s| {
        Uuid::parse_str(&s).map_err(|_| StoreError::Corrupt(format!("{col}: not a uuid: {s}")))
    })
    .transpose()
}

pub(crate) fn vocab<T: FromStr>(row: &Row<'_>, col: &str) -> Result<T> {
    let s: String = row.get(col)?;
    s.parse()
        .map_err(|_| StoreError::Corrupt(format!("{col}: unknown value {s}")))
}

pub(crate) fn opt_vocab<T: FromStr>(row: &Row<'_>, col: &str) -> Result<Option<T>> {
    let s: Option<String> = row.get(col)?;
    s.map(|s| {
        s.parse()
            .map_err(|_| StoreError::Corrupt(format!("{col}: unknown value {s}")))
    })
    .transpose()
}

pub(crate) fn json<T: DeserializeOwned>(row: &Row<'_>, col: &str) -> Result<T> {
    let s: String = row.get(col)?;
    Ok(serde_json::from_str(&s)?)
}

pub(crate) fn opt_json<T: DeserializeOwned>(row: &Row<'_>, col: &str) -> Result<Option<T>> {
    let s: Option<String> = row.get(col)?;
    s.map(|s| serde_json::from_str(&s).map_err(StoreError::from))
        .transpose()
}

pub(crate) fn to_json<T: Serialize>(value: &T) -> Result<String> {
    Ok(serde_json::to_string(value)?)
}

pub(crate) fn value_json(value: &Value) -> Result<String> {
    to_json(value)
}

pub(crate) fn budget_json(budget: Option<&saka_core::model::Budget>) -> Result<String> {
    match budget {
        Some(b) => to_json(b),
        None => Ok("{}".to_string()),
    }
}

pub(crate) fn budget_col(row: &Row<'_>, col: &str) -> Result<Option<saka_core::model::Budget>> {
    let v: Value = json(row, col)?;
    if v.as_object().is_some_and(serde_json::Map::is_empty) {
        return Ok(None);
    }
    Ok(Some(serde_json::from_value(v)?))
}

pub(crate) fn u32_col(row: &Row<'_>, col: &str) -> Result<u32> {
    let v: i64 = row.get(col)?;
    u32::try_from(v).map_err(|_| StoreError::Corrupt(format!("{col}: out of range: {v}")))
}

pub(crate) fn u64_col(row: &Row<'_>, col: &str) -> Result<u64> {
    let v: i64 = row.get(col)?;
    u64::try_from(v).map_err(|_| StoreError::Corrupt(format!("{col}: out of range: {v}")))
}

pub(crate) fn i64_of(v: u64) -> Result<i64> {
    i64::try_from(v).map_err(|_| StoreError::Corrupt(format!("value out of range: {v}")))
}
