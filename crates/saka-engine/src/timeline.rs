use saka_core::ids::PermissionRequestId;
use saka_core::model::{RunOwner, SessionActivity};
use saka_core::session::SessionInput;
use saka_core::time::Timestamp;
use saka_ports::{RunnerEvent, RunnerEventKind};
use serde_json::{Value, json};

pub(crate) const ACTIVITY_SUMMARY_MAX: usize = 500;

pub(crate) fn cut(text: &str) -> String {
    text.chars().take(ACTIVITY_SUMMARY_MAX).collect()
}

pub(crate) fn activity(
    event: &RunnerEvent,
    owner: RunOwner,
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
        owner,
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

fn window(name: &str, w: &Value, refused: bool) -> Option<SessionInput> {
    let utilization = w.get("utilization").and_then(Value::as_f64)?;
    Some(SessionInput::QuotaObserved {
        window: name.to_string(),
        utilization: utilization.max(0.0),
        status: if refused {
            "rejected"
        } else {
            w.get("status").and_then(Value::as_str).unwrap_or("allowed")
        }
        .to_string(),
        resets_at: resets_at(w),
        refused,
    })
}

fn resets_at(w: &Value) -> Option<Timestamp> {
    w.get("resetsAt")
        .or_else(|| w.get("resets_at"))
        .and_then(reset_ms)
}

// Only the top-level status says whether requests are refused; a window's own status
// is informational, so the block always lands on the limiting window, `rateLimitType`.
pub(crate) fn quota_windows(info: &Value) -> Vec<SessionInput> {
    let refused = info.get("status").and_then(Value::as_str) == Some("rejected");
    let limiting = info
        .get("rateLimitType")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let mut observed: Vec<SessionInput> =
        match info.get("unifiedWindows").and_then(Value::as_object) {
            Some(windows) => windows
                .iter()
                .filter_map(|(name, w)| window(name, w, refused && name == limiting))
                .collect(),
            None if info.get("rateLimitType").is_some() => {
                window(limiting, info, refused).into_iter().collect()
            }
            None => vec![],
        };
    let blocked = observed
        .iter()
        .any(|o| matches!(o, SessionInput::QuotaObserved { refused: true, .. }));
    if refused && !blocked {
        let reset = info
            .get("unifiedWindows")
            .and_then(|w| w.get(limiting))
            .and_then(resets_at);
        observed.push(SessionInput::QuotaObserved {
            window: limiting.to_string(),
            utilization: info
                .get("utilization")
                .and_then(Value::as_f64)
                .unwrap_or(1.0)
                .max(0.0),
            status: "rejected".to_string(),
            resets_at: resets_at(info).or(reset),
            refused: true,
        });
    }
    observed
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
        let owner = RunOwner::StageRun {
            work_item_id: saka_core::ids::WorkItemId(Uuid::from_u128(2)),
            stage_run_id: saka_core::ids::StageRunId(Uuid::from_u128(5)),
        };
        let perm = PermissionRequestId(Uuid::from_u128(3));
        let row = |kind, p| activity(&event(kind), owner, p).map(|a| (a.kind, a.summary, a.data));
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

    fn observed(
        window: &str,
        utilization: f64,
        status: &str,
        reset_s: i64,
        refused: bool,
    ) -> SessionInput {
        SessionInput::QuotaObserved {
            window: window.into(),
            utilization,
            status: status.into(),
            resets_at: Some(Timestamp(reset_s * 1000)),
            refused,
        }
    }

    #[test]
    fn a_rate_limit_event_from_claude_code_gives_its_windows() {
        let sample = json!({
            "status": "allowed", "resetsAt": 1_793_786_400_i64, "rateLimitType": "seven_day",
            "overageStatus": "rejected", "overageDisabledReason": "org_level_disabled",
            "isUsingOverage": false,
            "unifiedWindows": {
                "seven_day": { "utilization": 0.58, "resetsAt": 1_793_786_400_i64, "status": "allowed" },
            },
        });
        assert_eq!(
            quota_windows(&sample),
            vec![observed("seven_day", 0.58, "allowed", 1_793_786_400, false)]
        );
    }

    #[test]
    fn only_the_top_level_status_refuses_and_only_on_the_limiting_window() {
        let windows = json!({
            "five_hour": { "utilization": 1.0, "resetsAt": 1_800_000_000, "status": "rejected" },
            "seven_day": { "utilization": 0.4, "resetsAt": 1_800_500_000, "status": "allowed" },
        });
        let allowed =
            json!({ "status": "allowed", "rateLimitType": "five_hour", "unifiedWindows": windows });
        assert_eq!(
            quota_windows(&allowed),
            vec![
                observed("five_hour", 1.0, "rejected", 1_800_000_000, false),
                observed("seven_day", 0.4, "allowed", 1_800_500_000, false),
            ]
        );
        let rejected = json!({ "status": "rejected", "rateLimitType": "seven_day", "unifiedWindows": windows });
        assert_eq!(
            quota_windows(&rejected),
            vec![
                observed("five_hour", 1.0, "rejected", 1_800_000_000, false),
                observed("seven_day", 0.4, "rejected", 1_800_500_000, true),
            ]
        );
    }

    #[test]
    fn a_top_level_refusal_is_never_lost() {
        let bare = json!({ "status": "rejected", "rateLimitType": "five_hour", "resetsAt": 1_800_000_000 });
        assert_eq!(
            quota_windows(&bare),
            vec![observed("five_hour", 1.0, "rejected", 1_800_000_000, true)]
        );
        let elsewhere = json!({
            "status": "rejected", "rateLimitType": "five_hour", "resetsAt": 1_800_000_000,
            "unifiedWindows": { "seven_day": { "utilization": 0.4, "resetsAt": 1_800_500_000 } },
        });
        assert_eq!(
            quota_windows(&elsewhere),
            vec![
                observed("seven_day", 0.4, "allowed", 1_800_500_000, false),
                observed("five_hour", 1.0, "rejected", 1_800_000_000, true),
            ]
        );
        assert_eq!(
            quota_windows(&json!({ "status": "rejected", "utilization": 0.97 })),
            vec![SessionInput::QuotaObserved {
                window: "unknown".into(),
                utilization: 0.97,
                status: "rejected".into(),
                resets_at: None,
                refused: true,
            }]
        );
    }

    #[test]
    fn a_rate_limit_without_numbers_observes_nothing() {
        assert!(quota_windows(&json!({ "status": "allowed" })).is_empty());
        assert!(quota_windows(&json!({ "status": "allowed", "utilization": 0.5 })).is_empty());
        let single = json!({ "status": "allowed_warning", "rateLimitType": "seven_day", "utilization": 0.81, "resetsAt": 1_800_000_000 });
        assert_eq!(
            quota_windows(&single),
            vec![observed(
                "seven_day",
                0.81,
                "allowed_warning",
                1_800_000_000,
                false
            )]
        );
    }
}
