use crate::Ctx;
use crate::event::Event;
use crate::ids::{ChangeRequestId, ProjectId, RepoId, SpecId, StageRunId, WorkItemId};
use crate::model::{SpecLink, Task};
use crate::rejection::Rejection;
use crate::task::TaskInput;
use crate::vocab::{
    BlockerResolution, ChangeRole, ChangeState, ExternalCause, SessionState, Stage, StagePhase,
    StageRunState, Trigger, WorkItemState,
};
use crate::workflow::Workflow;

pub const MAX_TITLE_CHARS: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Incoming {
    pub project_id: ProjectId,
    pub repo_id: RepoId,
    pub repo_name: String,
    pub number: u64,
    pub title: String,
    pub url: String,
    pub head_branch: String,
    pub base_branch: String,
    pub head_sha: String,
    pub author: String,
    pub from_fork: bool,
    pub draft: bool,
    pub spec: Option<SpecLink>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Review,
    Fix,
    Merge,
}

impl Command {
    pub fn stage(self) -> Stage {
        match self {
            Command::Review => Stage::Review,
            Command::Fix => Stage::Fix,
            Command::Merge => Stage::Merge,
        }
    }
}

fn command_words(body: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut fenced = false;
    for line in body.lines() {
        let line = line.trim_start();
        if line.starts_with("```") || line.starts_with("~~~") {
            fenced = !fenced;
            continue;
        }
        if fenced || line.starts_with('>') {
            continue;
        }
        let mut words = line.split_whitespace();
        let (Some(prefix), Some(word)) = (words.next(), words.next()) else {
            continue;
        };
        if prefix.eq_ignore_ascii_case("/saka") {
            found.push(word.to_ascii_lowercase());
        }
    }
    found
}

pub fn parse_commands(body: &str) -> Vec<Command> {
    command_words(body)
        .iter()
        .filter_map(|word| match word.as_str() {
            "review" => Some(Command::Review),
            "fix" => Some(Command::Fix),
            "merge" => Some(Command::Merge),
            _ => None,
        })
        .collect()
}

pub fn asks_intake(body: &str) -> bool {
    command_words(body).iter().any(|word| word == "intake")
}

pub fn task_key_tokens(text: &str) -> Vec<(String, u32)> {
    text.split(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '/')))
        .filter_map(|word| {
            let (slug, step) = word.rsplit_once('/')?;
            let digits = step.strip_prefix('s').or_else(|| step.strip_prefix('S'))?;
            let n: u32 = digits.parse().ok().filter(|n| *n >= 1)?;
            let slug = slug.trim_matches('/');
            (!slug.is_empty()).then(|| (slug.to_string(), n))
        })
        .collect()
}

pub fn link_spec(
    text: &str,
    specs: &[(SpecId, String)],
    step_exists: impl Fn(SpecId, u32) -> bool,
) -> Option<SpecLink> {
    task_key_tokens(text).into_iter().find_map(|(slug, n)| {
        let (spec_id, _) = specs.iter().find(|(_, s)| *s == slug)?;
        Some(SpecLink {
            spec_id: *spec_id,
            step_id: step_exists(*spec_id, n).then(|| format!("S{n}")),
        })
    })
}

pub fn task_key(repo_name: &str, number: u64) -> String {
    format!("{repo_name}#{number}")
}

fn clipped_title(title: &str) -> String {
    let cut: String = title.trim().chars().take(MAX_TITLE_CHARS).collect();
    if cut.is_empty() {
        "(untitled)".to_string()
    } else {
        cut
    }
}

pub(crate) fn reason(cause: ExternalCause, actor: Option<&str>) -> Option<String> {
    let who = actor.unwrap_or("unknown");
    match cause {
        ExternalCause::Auto => None,
        ExternalCause::Comment => Some(format!("comment by {who}")),
        ExternalCause::Label => Some(format!("label by {who}")),
        ExternalCause::Operator => Some("operator".to_string()),
    }
}

pub(crate) fn review_run(
    task_id: WorkItemId,
    attempt: u32,
    head_sha: &str,
    cause: ExternalCause,
    actor: Option<&str>,
    ctx: &mut Ctx<'_>,
) -> Event {
    Event::StageRunCreated {
        work_item_id: task_id,
        stage_run_id: StageRunId::new(ctx.ids),
        stage: Stage::Review,
        attempt,
        trigger: if cause == ExternalCause::Auto {
            Trigger::Auto
        } else {
            Trigger::Manual
        },
        entry_phase: StagePhase::AwaitingReview,
        reason: reason(cause, actor),
        retry_of: None,
        budget: None,
        head_sha: Some(head_sha.to_string()),
        fix: None,
    }
}

pub fn accept(
    workflow: &Workflow,
    incoming: Incoming,
    cause: ExternalCause,
    actor: Option<String>,
    ctx: &mut Ctx<'_>,
) -> Result<Vec<Event>, Rejection> {
    if workflow.change_request.is_none() {
        return Err(Rejection::EntryPointDisabled {
            entry: "change_request",
        });
    }
    let Some(review) = workflow.stage_trigger(Stage::Review) else {
        return Err(Rejection::StageNotInWorkflow { stage: "review" });
    };
    let work_item_id = WorkItemId::new(ctx.ids);
    let mut events = vec![Event::ExternalChangeAccepted {
        work_item_id,
        project_id: incoming.project_id,
        repo_id: incoming.repo_id,
        task_key: task_key(&incoming.repo_name, incoming.number),
        title: clipped_title(&incoming.title),
        branch: incoming.head_branch,
        change_request_id: ChangeRequestId::new(ctx.ids),
        number: incoming.number,
        url: incoming.url,
        base_branch: incoming.base_branch,
        head_sha: incoming.head_sha.clone(),
        author: incoming.author,
        from_fork: incoming.from_fork,
        draft: incoming.draft,
        spec: incoming.spec,
        cause,
        actor: actor.clone(),
    }];
    if cause != ExternalCause::Auto || review == Trigger::Auto {
        events.push(review_run(
            work_item_id,
            1,
            &incoming.head_sha,
            cause,
            actor.as_deref(),
            ctx,
        ));
    }
    Ok(events)
}

pub fn merging(task: &Task) -> bool {
    task.external.is_some()
        && matches!(
            task.item.state,
            WorkItemState::InStage | WorkItemState::Blocked
        )
        && task
            .stage_run
            .as_ref()
            .is_some_and(|r| r.stage == Stage::Merge)
}

fn open_run(task: &Task) -> Option<StageRunId> {
    task.stage_run
        .as_ref()
        .filter(|r| {
            matches!(r.stage, Stage::Review | Stage::Fix | Stage::Merge)
                && matches!(r.state, StageRunState::Pending | StageRunState::Running)
        })
        .map(|r| r.id)
}

pub fn cancel_run(task: &Task, reason: &str) -> Option<Event> {
    open_run(task).map(|stage_run_id| Event::StageRunCancelled {
        work_item_id: task.item.id,
        stage_run_id,
        reason: reason.to_string(),
    })
}

fn cancel_open(task: &Task, reason: &str, now: crate::time::Timestamp) -> Vec<Event> {
    let mut events: Vec<Event> = cancel_run(task, reason).into_iter().collect();
    if task
        .stage_run
        .as_ref()
        .is_some_and(|r| matches!(r.stage, Stage::Review | Stage::Fix))
        && let Some(view) = crate::task::run_view(task)
    {
        events.extend(crate::session::end(
            &view,
            SessionState::Ended,
            reason,
            serde_json::json!({}),
            now,
        ));
    }
    events
}

fn release(task: &Task, reason: &str, now: crate::time::Timestamp) -> Vec<Event> {
    let mut events = cancel_open(task, reason, now);
    if events.is_empty()
        && task.is_at(Stage::Review, StagePhase::Reviewed)
        && let Some(run) = &task.stage_run
    {
        events.push(Event::StageRunCancelled {
            work_item_id: task.item.id,
            stage_run_id: run.id,
            reason: reason.to_string(),
        });
    }
    events
}

fn request(
    task: &Task,
    head_sha: &str,
    cause: ExternalCause,
    actor: Option<&str>,
    ctx: &mut Ctx<'_>,
) -> Result<Vec<Event>, Rejection> {
    let Some(external) = &task.external else {
        return Err(Rejection::Unsupported);
    };
    let fixing = task
        .stage_run
        .as_ref()
        .is_some_and(|r| r.stage == Stage::Fix && task.item.state != WorkItemState::Blocked);
    if !task.is_at_stage(Stage::Review) && !fixing {
        return Err(Rejection::InvalidState {
            state: task.item.state.as_str().to_string(),
        });
    }
    let Some(cr) = &task.change_request else {
        return Err(Rejection::Unsupported);
    };
    if cr.state != ChangeState::Open {
        return Err(Rejection::ChangeRequestNotOpen {
            number: cr.number,
            state: cr.state.as_str(),
        });
    }
    if !external.workflow.has_stage(Stage::Review) {
        return Err(Rejection::StageNotInWorkflow { stage: "review" });
    }
    let already = task.stage_run.as_ref().is_some_and(|r| {
        r.stage == Stage::Review
            && r.state != StageRunState::Cancelled
            && r.head_sha.as_deref() == Some(head_sha)
    });
    if already {
        return Err(Rejection::ReviewAlreadyRequested {
            head_sha: head_sha.to_string(),
        });
    }
    let mut events = Vec::new();
    if cr.head_sha != head_sha {
        events.push(Event::ChangeRequestUpdated {
            work_item_id: task.item.id,
            change_request_id: cr.id,
            role: ChangeRole::Code,
            head_sha: head_sha.to_string(),
        });
    }
    events.extend(cancel_open(task, "head_moved", ctx.now));
    let attempt = task.attempts.get(&Stage::Review).copied().unwrap_or(0) + 1;
    events.push(review_run(
        task.item.id,
        attempt,
        head_sha,
        cause,
        actor,
        ctx,
    ));
    Ok(events)
}

fn observe(
    task: &Task,
    state: ChangeState,
    head_sha: &str,
    merge_commit_sha: Option<String>,
    merged_at: Option<crate::time::Timestamp>,
    ctx: &mut Ctx<'_>,
) -> Vec<Event> {
    let (Some(cr), Some(external)) = (&task.change_request, &task.external) else {
        return Vec::new();
    };
    let blocked = task.item.state == WorkItemState::Blocked;
    if cr.state != ChangeState::Open || !(blocked || task.item.state == WorkItemState::InStage) {
        return Vec::new();
    }
    let id = task.item.id;
    let updated = || Event::ChangeRequestUpdated {
        work_item_id: id,
        change_request_id: cr.id,
        role: ChangeRole::Code,
        head_sha: head_sha.to_string(),
    };
    let mut events = Vec::new();
    if let (true, ChangeState::Merged | ChangeState::Closed, Some(b)) =
        (blocked, state, task.open_blocker())
    {
        events.push(Event::BlockerResolved {
            work_item_id: id,
            blocker_id: b.id,
            resolution: BlockerResolution::Auto,
            reason: None,
        });
    }
    match state {
        ChangeState::Merged => {
            let Some(merge_commit_sha) = merge_commit_sha else {
                return Vec::new();
            };
            events.push(Event::ChangeRequestMerged {
                work_item_id: id,
                change_request_id: cr.id,
                role: ChangeRole::Code,
                merge_commit_sha: merge_commit_sha.clone(),
                merged_at,
                merged_by_saka: false,
            });
            events.extend(cancel_open(task, "change_merged", ctx.now));
            events.push(Event::WorkItemDone {
                work_item_id: id,
                merge_commit_sha,
            });
            events
        }
        ChangeState::Closed => {
            events.push(Event::ChangeRequestClosed {
                work_item_id: id,
                change_request_id: cr.id,
                role: ChangeRole::Code,
            });
            events.extend(cancel_open(task, "change_closed", ctx.now));
            events.push(Event::WorkItemCancelled {
                work_item_id: id,
                reason: "change_closed".to_string(),
            });
            events
        }
        ChangeState::Open if head_sha != cr.head_sha && blocked => vec![updated()],
        ChangeState::Open if head_sha != cr.head_sha => {
            match external.workflow.stage_trigger(Stage::Review) {
                Some(Trigger::Auto) => {
                    request(task, head_sha, ExternalCause::Auto, None, ctx).unwrap_or_default()
                }
                Some(Trigger::Manual) => {
                    let mut events = vec![updated()];
                    events.extend(release(task, "head_moved", ctx.now));
                    events
                }
                None => vec![updated()],
            }
        }
        ChangeState::Open => Vec::new(),
    }
}

pub fn decide(task: &Task, input: TaskInput, ctx: &mut Ctx<'_>) -> Result<Vec<Event>, Rejection> {
    match input {
        TaskInput::ChangeObserved {
            role: ChangeRole::Code,
            state,
            head_sha,
            merge_commit_sha,
            merged_at,
            ..
        } => Ok(observe(
            task,
            state,
            &head_sha,
            merge_commit_sha,
            merged_at,
            ctx,
        )),
        TaskInput::ReviewRequested {
            head_sha,
            cause,
            actor,
        } => request(task, &head_sha, cause, actor.as_deref(), ctx),
        _ => Err(Rejection::Unsupported),
    }
}

pub fn fix_blocker(task: &Task) -> Option<&'static str> {
    let external = task.external.as_ref()?;
    if external.from_fork {
        return Some("fork");
    }
    if !task.workflow.has_stage(Stage::Fix) {
        return Some("not_in_workflow");
    }
    let rejected = task
        .change_request
        .as_ref()
        .is_some_and(|cr| task.rejected_at(&cr.head_sha).is_some());
    if !task.is_at(Stage::Review, StagePhase::Reviewed) || !rejected {
        return Some("no_findings");
    }
    (!task.fix_rounds_left()).then_some("rounds_exhausted")
}

pub fn fix_available(task: &Task) -> bool {
    task.external.is_some()
        && task
            .change_request
            .as_ref()
            .is_some_and(|c| c.state == ChangeState::Open)
        && fix_blocker(task).is_none()
}

pub fn awaiting_request(task: &Task) -> bool {
    task.external.is_some()
        && task.is_at_stage(Stage::Review)
        && task
            .change_request
            .as_ref()
            .is_some_and(|c| c.state == ChangeState::Open)
        && !task.stage_run.as_ref().is_some_and(|r| {
            r.stage == Stage::Review
                && r.state != StageRunState::Cancelled
                && task
                    .change_request
                    .as_ref()
                    .is_some_and(|c| r.head_sha.as_deref() == Some(c.head_sha.as_str()))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_are_read_from_the_start_of_a_line_only() {
        assert_eq!(parse_commands("/saka review"), [Command::Review]);
        assert_eq!(parse_commands("  /SAKA Review please"), [Command::Review]);
        assert_eq!(
            parse_commands("thanks!\n/saka fix\n/saka merge"),
            [Command::Fix, Command::Merge]
        );
        for text in [
            "please run /saka review",
            "> /saka review",
            "```\n/saka review\n```",
            "~~~\n/saka review\n~~~",
            "/saka",
            "/sakareview",
            "/saka nope",
            "",
        ] {
            assert!(parse_commands(text).is_empty(), "{text:?}");
        }
        assert_eq!(
            parse_commands("```\n/saka fix\n```\n/saka review"),
            [Command::Review],
            "a command after a closed fence counts"
        );
    }

    #[test]
    fn the_intake_command_follows_the_same_rules_as_the_review_commands() {
        for text in [
            "/saka intake",
            "  /SAKA Intake please",
            "thanks\n/saka intake",
        ] {
            assert!(asks_intake(text), "{text:?}");
        }
        for text in [
            "please run /saka intake",
            "> /saka intake",
            "```\n/saka intake\n```",
            "/sakaintake",
            "/saka review",
            "",
        ] {
            assert!(!asks_intake(text), "{text:?}");
        }
        assert!(parse_commands("/saka intake").is_empty());
    }

    #[test]
    fn a_command_names_the_stage_it_asks_for() {
        assert_eq!(Command::Review.stage(), Stage::Review);
        assert_eq!(Command::Fix.stage(), Stage::Fix);
        assert_eq!(Command::Merge.stage(), Stage::Merge);
    }

    #[test]
    fn task_keys_are_found_in_prose_and_links() {
        assert_eq!(
            task_key_tokens("Implements tasks-done/s2 (see https://x/y)."),
            [("tasks-done".to_string(), 2)]
        );
        assert_eq!(
            task_key_tokens("<!-- saka-task: two-step/S1 -->"),
            [("two-step".to_string(), 1)]
        );
        assert!(task_key_tokens("a/b and docs/s0 and s3").is_empty());
    }

    #[test]
    fn a_known_slug_links_the_spec_and_the_step_only_when_it_exists() {
        let spec = SpecId(uuid::Uuid::from_u128(1));
        let specs = vec![(spec, "tasks-done".to_string())];
        let link = link_spec("Closes tasks-done/s2", &specs, |_, n| n == 2).unwrap();
        assert_eq!(link.spec_id, spec);
        assert_eq!(link.step_id.as_deref(), Some("S2"));
        let no_step = link_spec("Closes tasks-done/s9", &specs, |_, n| n == 2).unwrap();
        assert_eq!(no_step.step_id, None);
        assert!(link_spec("Closes other/s1", &specs, |_, _| true).is_none());
        assert!(link_spec("no key here", &specs, |_, _| true).is_none());
    }

    #[test]
    fn titles_are_clipped_and_never_empty() {
        assert_eq!(clipped_title("  Add retries "), "Add retries");
        assert_eq!(
            clipped_title(&"é".repeat(300)).chars().count(),
            MAX_TITLE_CHARS
        );
        assert_eq!(clipped_title("   "), "(untitled)");
        assert_eq!(task_key("api", 42), "api#42");
    }

    #[test]
    fn a_request_reason_names_who_asked() {
        assert_eq!(reason(ExternalCause::Auto, None), None);
        assert_eq!(
            reason(ExternalCause::Comment, Some("bob")).as_deref(),
            Some("comment by bob")
        );
        assert_eq!(
            reason(ExternalCause::Label, Some("carol")).as_deref(),
            Some("label by carol")
        );
        assert_eq!(
            reason(ExternalCause::Operator, None).as_deref(),
            Some("operator")
        );
    }
}
