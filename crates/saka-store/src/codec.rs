use std::str::FromStr;

use rusqlite::Row;
use saka_core::ids::{SpecDraftAttemptId, SpecDraftId, StageRunId, WorkItemId};
use saka_core::model::RunOwner;
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

pub(crate) fn run_owner(row: &Row<'_>) -> Result<RunOwner> {
    let task = (
        opt_uuid(row, "work_item_id")?,
        opt_uuid(row, "stage_run_id")?,
    );
    let draft = (
        opt_uuid(row, "spec_draft_id")?,
        opt_uuid(row, "spec_draft_attempt_id")?,
    );
    match (task, draft) {
        ((Some(work_item), Some(run)), (None, None)) => Ok(RunOwner::StageRun {
            work_item_id: WorkItemId(work_item),
            stage_run_id: StageRunId(run),
        }),
        ((None, None), (Some(draft), Some(attempt))) => Ok(RunOwner::DraftAttempt {
            spec_draft_id: SpecDraftId(draft),
            attempt_id: SpecDraftAttemptId(attempt),
        }),
        _ => Err(StoreError::Corrupt("run owner".into())),
    }
}

pub(crate) struct OwnerColumns {
    pub work_item_id: Option<String>,
    pub stage_run_id: Option<String>,
    pub spec_draft_id: Option<String>,
    pub spec_draft_attempt_id: Option<String>,
}

pub(crate) fn owner_columns(owner: &RunOwner) -> OwnerColumns {
    OwnerColumns {
        work_item_id: owner.work_item_id().map(|id| id.to_string()),
        stage_run_id: owner.stage_run_id().map(|id| id.to_string()),
        spec_draft_id: owner.spec_draft_id().map(|id| id.to_string()),
        spec_draft_attempt_id: owner.attempt_id().map(|id| id.to_string()),
    }
}
