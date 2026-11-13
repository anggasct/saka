use rusqlite::Connection;
use saka_core::ids::{StageRunId, WorkItemId};
use saka_core::model::FixRound;
use saka_core::vocab::Stage;

use crate::codec::{opt_json, uuid, vocab};
use crate::error::Result;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunSummary {
    pub stage_run_id: StageRunId,
    pub stage: Stage,
    pub fix_round: Option<u32>,
    pub text: Option<String>,
}

pub fn run_summaries(conn: &Connection, work_item: WorkItemId) -> Result<Vec<RunSummary>> {
    let mut stmt = conn.prepare(
        "SELECT s.stage_run_id AS stage_run_id, r.stage AS stage, r.fix AS fix,
                json_extract(a.data, '$.agent_summary') AS summary
           FROM session_activity a
           JOIN session s ON s.id = a.session_id
           JOIN stage_run r ON r.id = s.stage_run_id
          WHERE a.work_item_id = ?1 AND a.kind = 'result'
            AND json_extract(a.data, '$.status') = 'success'
            AND r.stage IN ('dev', 'fix')
          ORDER BY a.seq",
    )?;
    let mut rows = stmt.query([work_item.0.to_string()])?;
    let mut out: Vec<RunSummary> = Vec::new();
    while let Some(row) = rows.next()? {
        let stage_run_id = StageRunId(uuid(row, "stage_run_id")?);
        let text: Option<String> = row.get("summary")?;
        match out.iter_mut().find(|r| r.stage_run_id == stage_run_id) {
            Some(run) => run.text = text.or(run.text.take()),
            None => out.push(RunSummary {
                stage_run_id,
                stage: vocab(row, "stage")?,
                fix_round: opt_json::<FixRound>(row, "fix")?.map(|f| f.number),
                text,
            }),
        }
    }
    Ok(out)
}
