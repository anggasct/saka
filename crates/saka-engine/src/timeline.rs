use saka_core::ids::{PermissionRequestId, WorkItemId};
use saka_core::model::SessionActivity;
use saka_core::task::TaskInput;
use saka_core::time::Timestamp;
use saka_ports::{RunnerEvent, RunnerEventKind};
use serde_json::{Value, json};

pub(crate) const ACTIVITY_SUMMARY_MAX: usize = 500;

pub(crate) fn cut(text: &str) -> String {
    text.chars().take(ACTIVITY_SUMMARY_MAX).collect()
}

pub(crate) fn activity(
    event: &RunnerEvent,
    work_item_id: WorkItemId,
    permission: Option<(PermissionRequestId, &str, &str)>,
) -> Option<SessionActivity> {
    let system = |code: &str, params: Value| {
        (
            "system",
            code.to_string(),
            json!({ "code": code, "params": params }),
        )
    };
    let (kind, summary, data) = match &event.kind {
        RunnerEventKind::CliStarted { pid, cli_version } => system(
            "cli_started",
            json!({ "pid": pid, "cli_version": cli_version }),
        ),
        RunnerEventKind::SessionInit { model, .. } => {
            system("session_init", json!({ "model": model }))
        }
        RunnerEventKind::CliExited { code, signal } => {
            system("cli_exited", json!({ "code": code, "signal": signal }))
        }
        RunnerEventKind::RunnerError { code, params } => system(code, params.clone()),
        RunnerEventKind::Activity {
            category,
            summary,
            tool,
            is_error,
        } => match category.as_str() {
            "thinking" => ("thought", summary.clone(), json!({})),
            "text" => ("message", summary.clone(), json!({})),
            "tool_use" => ("action", summary.clone(), json!({ "tool": tool })),
            "tool_result" => (
                "output",
                summary.clone(),
                json!({ "is_error": is_error.unwrap_or(false) }),
            ),
            _ => return None,
        },
        RunnerEventKind::PermissionRequest { .. } | RunnerEventKind::PermissionDelivered { .. } => {
            let (id, summary, state) = permission?;
            (
                "permission",
                summary.to_string(),
                json!({ "permission_request_id": id, "state": state }),
            )
        }
        RunnerEventKind::TurnEnded {
            turn,
            status,
            result_summary,
            ..
        } => (
            "result",
            result_summary.clone().unwrap_or_default(),
            json!({ "turn": turn, "status": status }),
        ),
        RunnerEventKind::Steer { turn, text, .. } => {
            ("steer", text.clone(), json!({ "turn": turn }))
        }
        RunnerEventKind::RateLimit { .. } => return None,
    };
    Some(SessionActivity {
        session_id: event.session_id,
        offset: event.offset,
        work_item_id,
        at: event.at,
        kind: kind.to_string(),
        summary: cut(&summary),
        data,
    })
}

fn reset_ms(v: &Value) -> Option<Timestamp> {
    let n = v.as_f64()?;
    // The CLI reports seconds; a value this large is already milliseconds.
    let ms = if n < 1e12 { n * 1000.0 } else { n };
    (0.0..9.0e15).contains(&ms).then_some(Timestamp(ms as i64))
}

fn window(name: &str, w: &Value) -> Option<TaskInput> {
    let utilization = w.get("utilization").and_then(Value::as_f64)?;
    Some(TaskInput::QuotaObserved {
        window: name.to_string(),
        utilization: utilization.max(0.0),
        status: w
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("allowed")
            .to_string(),
        resets_at: w
            .get("resets_at")
            .or_else(|| w.get("resetsAt"))
            .and_then(reset_ms),
    })
}

pub(crate) fn quota_windows(info: &Value) -> Vec<TaskInput> {
    if let Some(windows) = info.get("unifiedWindows").and_then(Value::as_object) {
        return windows
            .iter()
            .filter_map(|(name, w)| window(name, w))
            .collect();
    }
    info.get("rateLimitType")
        .and_then(Value::as_str)
        .and_then(|name| window(name, info))
        .into_iter()
        .collect()
}

#[cfg(test)]
mod tests {
    use saka_core::ids::SessionId;
    use uuid::Uuid;

    use super::*;

    fn event(kind: RunnerEventKind) -> RunnerEvent {
        RunnerEvent {
            session_id: SessionId(Uuid::from_u128(1)),
            offset: 4,
            at: Timestamp(9),
            kind,
        }
    }

    #[test]
    fn every_runner_event_kind_maps_to_its_timeline_row() {
        let item = WorkItemId(Uuid::from_u128(2));
        let perm = PermissionRequestId(Uuid::from_u128(3));
        let row = |kind, p| activity(&event(kind), item, p).map(|a| (a.kind, a.summary, a.data));
        let act = |category: &str, summary: &str, tool: Option<&str>, is_error: Option<bool>| {
            RunnerEventKind::Activity {
                category: category.into(),
                summary: summary.into(),
                tool: tool.map(String::from),
                is_error,
            }
        };
        assert_eq!(
            row(
                RunnerEventKind::SessionInit {
                    cli_session_id: "c".into(),
                    model: Some("m".into())
                },
                None
            ),
            Some((
                "system".into(),
                "session_init".into(),
                json!({ "code": "session_init", "params": { "model": "m" } })
            ))
        );
        assert_eq!(
            row(act("thinking", "hmm", None, None), None).map(|r| r.0),
            Some("thought".into())
        );
        assert_eq!(
            row(act("text", "done", None, None), None).map(|r| r.0),
            Some("message".into())
        );
        assert_eq!(
            row(act("tool_use", "Bash: ls", Some("Bash"), None), None),
            Some((
                "action".into(),
                "Bash: ls".into(),
                json!({ "tool": "Bash" })
            ))
        );
        assert_eq!(
            row(act("tool_result", "ok", None, Some(true)), None),
            Some(("output".into(), "ok".into(), json!({ "is_error": true })))
        );
        assert_eq!(row(act("other", "", None, None), None), None);
        assert_eq!(
            row(
                RunnerEventKind::PermissionRequest {
                    request_id: "r".into(),
                    tool: "Bash".into(),
                    input: json!({})
                },
                Some((perm, "Bash: npm i", "pending"))
            ),
            Some((
                "permission".into(),
                "Bash: npm i".into(),
                json!({ "permission_request_id": perm, "state": "pending" })
            ))
        );
        assert_eq!(
            row(
                RunnerEventKind::TurnEnded {
                    turn: 2,
                    status: "success".into(),
                    cost_usd_micros: None,
                    input_tokens: None,
                    output_tokens: None,
                    result_summary: Some("x".repeat(900)),
                },
                None
            )
            .map(|r| (r.0, r.1.chars().count(), r.2)),
            Some((
                "result".into(),
                500,
                json!({ "turn": 2, "status": "success" })
            ))
        );
        assert_eq!(
            row(
                RunnerEventKind::Steer {
                    n: 1,
                    turn: 3,
                    text: "go".into()
                },
                None
            ),
            Some(("steer".into(), "go".into(), json!({ "turn": 3 })))
        );
        assert_eq!(
            row(
                RunnerEventKind::CliExited {
                    code: Some(1),
                    signal: None
                },
                None
            )
            .map(|r| r.0),
            Some("system".into())
        );
        assert_eq!(
            row(RunnerEventKind::RateLimit { info: json!({}) }, None),
            None
        );
    }

    #[test]
    fn rate_limit_info_gives_one_observation_per_window() {
        let unified = json!({ "unifiedWindows": {
            "five_hour": { "utilization": 0.5, "status": "allowed", "resets_at": 1_800_000_000 },
            "seven_day": { "utilization": 0.95, "status": "rejected", "resets_at": 1_800_000_000_000_i64 },
        }});
        assert_eq!(
            quota_windows(&unified),
            vec![
                TaskInput::QuotaObserved {
                    window: "five_hour".into(),
                    utilization: 0.5,
                    status: "allowed".into(),
                    resets_at: Some(Timestamp(1_800_000_000_000)),
                },
                TaskInput::QuotaObserved {
                    window: "seven_day".into(),
                    utilization: 0.95,
                    status: "rejected".into(),
                    resets_at: Some(Timestamp(1_800_000_000_000)),
                },
            ]
        );
        let single = json!({ "status": "allowed_warning", "rateLimitType": "seven_day", "utilization": 0.81, "resetsAt": 1_800_000_000 });
        assert_eq!(quota_windows(&single).len(), 1);
        assert!(quota_windows(&json!({ "status": "allowed" })).is_empty());
    }
}
