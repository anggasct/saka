use std::collections::BTreeMap;

use saka_core::Ctx;
use saka_core::admission::Waiting;
use saka_core::draft::{self as core, CommitCheck, DraftInput, NewDraft, PacketFacts};
use saka_core::event::{Event, Prompt};
use saka_core::ids::{EnvironmentId, ProjectId, SessionId, SpecDraftAttemptId, SpecDraftId};
use saka_core::model::{
    Budget, Draft, DraftDocument, Environment, LintRound, Project, RunOwner, Usage,
};
use saka_core::paths::{self, MAX_DOC_BYTES};
use saka_core::rejection::{FileUnsupported, Rejection, TargetTakenBy};
use saka_core::vocab::{
    DraftFailureCode, DraftLocale, DraftMode, LintOutcome, PrimaryAction, SpecDraftState,
    SpecOrigin, TargetKind, WorkKind,
};
use saka_ports::{
    Blob, Checkout, DocBlob, DocExpect, DocFileWrite, DocFilesCommit, DocFilesWrite, EnvRef,
    IdsSource, LaunchProfile, PortError, PrepareRequest, RepoRef, SessionLaunch, WorktreeFile,
};
use saka_spec::{LintCode, LintIssue, PARSER_VERSION, content_hash};
use saka_store::draft as rows;
use saka_store::{Batch, DraftSet, list, read};
use serde_json::{Value, json};

use self::code::CodeSet;
use crate::action::{Action, Produced};
use crate::commands::{SpecFile, reason};
use crate::packet::{self, ArchitectInput, ArchitectPacket, PreviousAttempt};
use crate::register::{invalid, path_code};
use crate::views::{self, DecisionView, PermissionView};
use crate::worker::{Effect, Execution, field, internal, session_ref};
use crate::{Cause, Engine, EngineError, Extra, Mode, Result, Shows};

mod code;

pub const ARCHITECT_ACTOR: &str = "system:architect";
pub const IDEA_MAX: usize = 8000;
pub const NOTE_MAX: usize = 4000;
pub const MESSAGE_MAX: usize = 4000;
pub const COMMIT_MESSAGE_MAX: usize = 200;
pub const MAX_CONTEXT_PATHS: usize = 16;
pub const MAX_CONTEXT_BYTES: u64 = 256 * 1024;
pub const MAX_ROUND_BYTES: u64 = 256 * 1024;
pub const MAX_CHANGE_DIFF_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewSpecDraft {
    pub path: String,
    pub idea: String,
    pub locale: DraftLocale,
    pub mode: DraftMode,
    pub target_kind: TargetKind,
    pub kind: WorkKind,
    pub context_paths: Vec<String>,
    pub deployment_id: Option<saka_core::ids::DeploymentId>,
    pub incident_id: Option<saka_core::ids::IncidentId>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DraftChange {
    pub path: String,
    pub added: bool,
    pub diff: String,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DraftChangesView {
    pub files: Vec<DraftChange>,
    pub passed: bool,
    pub findings: Vec<Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DraftView {
    pub draft: Draft,
    pub waiting_on: Vec<Waiting>,
    pub primary_action: Option<PrimaryAction>,
    pub actions: Vec<&'static str>,
    pub last_round: Option<(u32, LintRound)>,
    pub max_rounds: u32,
    pub budget: Option<DraftBudgetView>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DraftBudgetView {
    pub attempt_id: SpecDraftAttemptId,
    pub warn_percent: u32,
    pub limit: Budget,
    pub used: Usage,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftContentView {
    pub attempt: u32,
    pub round: u32,
    pub path: String,
    pub bytes: u64,
    pub content_hash: Option<String>,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftPacketDocument {
    pub document: DraftDocument,
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftPacketView {
    pub attempt: u32,
    pub base_sha: String,
    pub sha256: String,
    pub documents: Vec<DraftPacketDocument>,
    pub text: String,
}

type Previous = (PreviousAttempt, Option<Vec<u8>>);

fn owner_of(d: &Draft) -> Option<RunOwner> {
    d.running_attempt().and_then(|_| d.run_owner())
}

fn bounded(value: &str, max: usize, field: &'static str) -> Result<String> {
    let trimmed = value.trim();
    let len = trimmed.chars().count();
    if len == 0 || len > max {
        return Err(invalid(field, "length"));
    }
    Ok(trimmed.to_string())
}

fn cwd_of(project_path: &str) -> String {
    match project_path.trim_matches('/') {
        "." => String::new(),
        dir => dir.to_string(),
    }
}

fn not_found(field: String) -> EngineError {
    EngineError::InvalidItem {
        field,
        code: "not_found".into(),
    }
}

fn issue_value(issue: &LintIssue) -> Value {
    serde_json::to_value(issue).unwrap_or_else(|_| json!({ "code": issue.code.as_str() }))
}

impl Engine {
    fn load_draft(&self, id: SpecDraftId) -> Result<Draft> {
        self.store
            .read(|c| rows::load_draft(c, id))?
            .ok_or(EngineError::Rejected(Rejection::NotFound))
    }

    pub fn spec_draft(&self, id: SpecDraftId) -> Result<DraftView> {
        let now = self.now();
        let (draft, drafts, quotas, last_round) = self.store.read(|c| {
            Ok((
                rows::load_draft(c, id)?,
                rows::load_drafts(c, DraftSet::Live)?,
                read::account_quotas(c)?,
                rows::last_round(c, id)?,
            ))
        })?;
        let draft = draft.ok_or(EngineError::Rejected(Rejection::NotFound))?;
        let waiting_on = core::waiting(&drafts, &quotas, &self.cfg.draft, now)
            .into_iter()
            .find(|(d, _)| *d == id)
            .map(|(_, w)| w)
            .unwrap_or_default();
        Ok(self.draft_view(draft, waiting_on, last_round))
    }

    pub fn draft_view(
        &self,
        draft: Draft,
        waiting_on: Vec<Waiting>,
        last_round: Option<(u32, LintRound)>,
    ) -> DraftView {
        let budget = draft.running_attempt().and_then(|a| {
            core::run_view(&draft).map(|run| DraftBudgetView {
                attempt_id: a.id,
                warn_percent: self.cfg.draft.session.budget_warn_percent,
                limit: a.budget,
                used: run.usage(self.now()),
            })
        });
        DraftView {
            primary_action: core::primary_action(&draft),
            actions: core::available_actions(&draft),
            waiting_on,
            last_round,
            max_rounds: self.cfg.draft.lint_rounds,
            budget,
            draft,
        }
    }

    pub fn spec_drafts(
        &self,
        project: ProjectId,
        states: &[String],
        page: &list::Page,
    ) -> Result<views::Paged<DraftView>> {
        let now = self.now();
        let (known, keys) = self.store.read(|c| {
            Ok((
                read::load_project(c, project)?.is_some(),
                list::spec_drafts(c, project, states, &views::probe(page))?,
            ))
        })?;
        if !known {
            return Err(Rejection::NotFound.into());
        }
        let keys = views::paged(keys, page, |(_, key)| *key);
        let next = keys.next;
        let keys = keys.items;
        let ids: Vec<SpecDraftId> = keys.iter().map(|(id, _)| *id).collect();
        let (mut drafts, live, quotas, rounds) = self.store.read(|c| {
            let mut rounds = BTreeMap::new();
            for id in &ids {
                if let Some(round) = rows::last_round(c, *id)? {
                    rounds.insert(*id, round);
                }
            }
            Ok((
                rows::load_drafts(c, DraftSet::Ids(&ids))?,
                rows::load_drafts(c, DraftSet::Live)?,
                read::account_quotas(c)?,
                rounds,
            ))
        })?;
        drafts.sort_by_key(|d| ids.iter().position(|id| *id == d.draft.id));
        let mut waiting: BTreeMap<SpecDraftId, Vec<Waiting>> =
            core::waiting(&live, &quotas, &self.cfg.draft, now)
                .into_iter()
                .collect();
        let mut rounds = rounds;
        Ok(views::Paged {
            items: drafts
                .into_iter()
                .map(|d| {
                    let id = d.draft.id;
                    self.draft_view(
                        d,
                        waiting.remove(&id).unwrap_or_default(),
                        rounds.remove(&id),
                    )
                })
                .collect(),
            next,
        })
    }

    pub fn spec_draft_rounds(&self, id: SpecDraftId) -> Result<Vec<(u32, LintRound)>> {
        let (draft, rounds) = self
            .store
            .read(|c| Ok((rows::load_draft(c, id)?, rows::lint_rounds(c, id)?)))?;
        draft.ok_or(EngineError::Rejected(Rejection::NotFound))?;
        Ok(rounds)
    }

    pub fn spec_draft_content(&self, id: SpecDraftId) -> Result<DraftContentView> {
        let (draft, written) = self
            .store
            .read(|c| Ok((rows::load_draft(c, id)?, rows::last_written(c, id)?)))?;
        let draft = draft.ok_or(EngineError::Rejected(Rejection::NotFound))?;
        let (attempt, round) = written.ok_or(EngineError::Rejected(Rejection::NotFound))?;
        let bytes = round.content.unwrap_or_default();
        let size = bytes.len() as u64;
        let content = String::from_utf8(bytes).map_err(|_| Rejection::FileUnsupported {
            reason: FileUnsupported::NotUtf8,
        })?;
        Ok(DraftContentView {
            attempt,
            round: round.round,
            path: draft.draft.path,
            bytes: size,
            content_hash: round.content_hash,
            content,
        })
    }

    pub fn spec_draft_packet(&self, id: SpecDraftId) -> Result<DraftPacketView> {
        let draft = self.load_draft(id)?;
        let no_packet = || {
            EngineError::Rejected(Rejection::InvalidState {
                state: draft.draft.state.as_str().to_string(),
            })
        };
        let attempt = draft.attempt.as_ref().ok_or_else(no_packet)?;
        let (text, rendered) = self.store.read(|c| {
            Ok((
                rows::packet(c, attempt.id)?,
                rows::packet_rendered(c, id, attempt.id)?,
            ))
        })?;
        let (Some(text), Some(rendered)) = (text, rendered) else {
            return Err(no_packet());
        };
        let documents: Vec<DraftDocument> =
            serde_json::from_value(rendered["documents"].clone())
                .map_err(|e| EngineError::Internal(format!("packet documents: {e}")))?;
        let sources = packet::sources(&text);
        Ok(DraftPacketView {
            attempt: attempt.attempt,
            base_sha: rendered["base_sha"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            sha256: rendered["packet_sha256"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            documents: documents
                .into_iter()
                .map(|document| DraftPacketDocument {
                    source: sources.get(&document.index).cloned().unwrap_or_default(),
                    document,
                })
                .collect(),
            text,
        })
    }

    pub(crate) fn decide_draft(
        &self,
        id: SpecDraftId,
        input: Option<DraftInput>,
        mode: Mode,
        cause: &Cause,
        extra: Extra<'_>,
    ) -> Result<Vec<Event>> {
        let _guard = self.locked()?;
        let now = self.now();
        let draft = self.load_draft(id)?;
        let (mut round, packet_text) = match &input {
            Some(DraftInput::Linted(r)) => (Some(r.clone()), None),
            Some(DraftInput::EnvironmentPrepared { packet, .. }) => {
                (None, Some(packet.text.clone()))
            }
            _ => (None, None),
        };
        let events = match input {
            Some(input) => {
                let mut ids = IdsSource(&*self.ports.ids);
                let mut ctx = Ctx { now, ids: &mut ids };
                match core::decide(&draft, input, &self.cfg.draft, &mut ctx) {
                    Ok(events) => events,
                    Err(rejection) if mode == Mode::Fact => {
                        tracing::debug!(spec_draft_id = %id, code = rejection.code(), "fact no longer applies");
                        Vec::new()
                    }
                    Err(rejection) => return Err(rejection.into()),
                }
            }
            None => Vec::new(),
        };
        let project = Some(draft.draft.project_id);
        let mut batch = Batch::new(now);
        batch.job_outcome = extra.outcome;
        batch
            .activities
            .extend(extra.activity.map(|a| (draft.draft.project_id, a)));
        batch.session_offsets.extend(extra.offset);
        let mut after = draft;
        for e in &events {
            after = core::evolve(Some(after), e, now).ok_or_else(|| {
                EngineError::Internal(format!("evolve dropped the draft on {}", e.kind()))
            })?;
            for job in core::react(&after, e, now) {
                self.push_job(&mut batch, job, project, cause);
            }
            crate::log_runtime(e, &cause.actor);
            self.observe_draft(e, &after);
            self.push_event(&mut batch, e, project, cause, now);
        }
        for e in &extra.events {
            self.push_event(&mut batch, e, project, cause, now);
        }
        for job in extra.jobs {
            self.push_job(&mut batch, job, project, cause);
        }
        for e in &events {
            match e {
                Event::SpecDraftLinted {
                    round: number,
                    attempt_id,
                    ..
                } => {
                    if let Some(mut r) = round.take().filter(|r| r.attempt_id == *attempt_id) {
                        r.round = *number;
                        r.created_at = now;
                        batch.draft_rounds.push(r);
                    }
                }
                Event::SpecDraftPacketRendered { attempt_id, .. } => {
                    if let Some(text) = &packet_text {
                        batch.draft_packets.push((*attempt_id, text.clone()));
                    }
                }
                _ => {}
            }
        }
        if let Some((action, shows)) = extra.action {
            let reply = match shows {
                Shows::Permission(pid) => {
                    let request = after
                        .permissions
                        .iter()
                        .find(|p| p.id == pid)
                        .cloned()
                        .ok_or_else(|| {
                            EngineError::Internal("answered request not in the draft".into())
                        })?;
                    let view = PermissionView {
                        request,
                        project_id: after.draft.project_id,
                    };
                    (action.respond)(Produced::Permission(&view))
                }
                Shows::Decision(did) => {
                    let request = after
                        .decisions
                        .iter()
                        .find(|d| d.id == did)
                        .cloned()
                        .ok_or_else(|| {
                            EngineError::Internal("answered question not in the draft".into())
                        })?;
                    let view = DecisionView {
                        request,
                        project_id: after.draft.project_id,
                    };
                    (action.respond)(Produced::Decision(&view))
                }
                Shows::Session(sid) => {
                    let session = after
                        .session
                        .iter()
                        .chain(after.retired_sessions.iter())
                        .find(|s| s.id == sid)
                        .ok_or_else(|| EngineError::Internal("session not in the draft".into()))?;
                    (action.respond)(Produced::Session(session))
                }
                Shows::Task | Shows::Taken(_) | Shows::Draft => {
                    (action.respond)(Produced::Draft(&after))
                }
            };
            batch.action = Some(action.record(self, cause, project, Some(id.0), reply));
        }
        if !events.is_empty() {
            batch.drafts.push(after);
        }
        self.commit(&batch)?;
        Ok(events)
    }

    fn observe_draft(&self, e: &Event, d: &Draft) {
        let attempt = d.draft.attempt;
        match e {
            Event::SpecDraftRequested { spec_draft_id, .. } => {
                tracing::info!(%spec_draft_id, attempt, "spec_draft.requested");
            }
            Event::SpecDraftAttemptStarted {
                spec_draft_id,
                trigger,
                ..
            } => {
                tracing::info!(%spec_draft_id, attempt, trigger = trigger.as_str(), "spec_draft.admitted");
            }
            Event::SpecDraftLinted {
                spec_draft_id,
                round,
                outcome,
                decisions_open,
                issue_codes,
                ..
            } => {
                self.counters.draft_lint_round(outcome.as_str());
                tracing::info!(
                    %spec_draft_id,
                    attempt,
                    round,
                    outcome = outcome.as_str(),
                    issues = issue_codes.len(),
                    decisions_open,
                    "spec_draft.linted"
                );
            }
            Event::SpecDraftSucceeded {
                spec_draft_id,
                spec_id,
                commit,
                ..
            } => {
                self.counters.draft_finished("succeeded");
                tracing::info!(%spec_draft_id, attempt, %spec_id, %commit, "spec_draft.succeeded");
            }
            Event::SpecDraftFailed {
                spec_draft_id,
                code,
                ..
            } => {
                self.counters.draft_finished("failed");
                let round = d.attempt.as_ref().map_or(0, |a| a.rounds);
                tracing::info!(%spec_draft_id, attempt, round, code = code.as_str(), "spec_draft.failed");
            }
            Event::SpecDraftCancelled { spec_draft_id, .. } => {
                self.counters.draft_finished("cancelled");
                tracing::info!(%spec_draft_id, attempt, "spec_draft.cancelled");
            }
            _ => {}
        }
    }

    pub async fn create_spec_draft(
        &self,
        project_id: ProjectId,
        new: NewSpecDraft,
        cause: &Cause,
        action: Option<&Action<'_>>,
    ) -> Result<Draft> {
        if new.idea.trim().is_empty() {
            return Err(invalid("idea", "required"));
        }
        if new.idea.chars().count() > IDEA_MAX {
            return Err(invalid("idea", "too_long"));
        }
        let (project, repo) = self.workspace_of(project_id)?;
        let path = new.path.clone();
        if path.is_empty() {
            return Err(invalid("path", "required"));
        }
        paths::check_doc_path(&project.project_path, &path)
            .map_err(|issue| invalid("path", path_code(issue)))?;
        paths::check_spec_path(&project.project_path, &path)
            .map_err(|issue| invalid("path", path_code(issue)))?;
        if new.context_paths.len() > MAX_CONTEXT_PATHS {
            return Err(invalid("context_paths", "too_many"));
        }
        for (i, p) in new.context_paths.iter().enumerate() {
            paths::check_doc_path(&project.project_path, p).map_err(|issue| {
                EngineError::InvalidItem {
                    field: format!("context_paths[{i}]"),
                    code: path_code(issue).into(),
                }
            })?;
        }
        for (i, p) in new.context_paths.iter().enumerate() {
            if new.context_paths[..i].contains(p) {
                return Err(invalid("context_paths", "duplicate"));
            }
        }
        if new.context_paths.contains(&path) {
            return Err(invalid("context_paths", "same_as_path"));
        }
        let opened = self
            .ports
            .docs
            .open(&repo, &project.base_branch, &path, MAX_DOC_BYTES)
            .await?;
        let file_name = path.rsplit('/').next().unwrap_or(&path);
        let slug = file_name
            .strip_suffix(".md")
            .or_else(|| file_name.strip_suffix(".markdown"))
            .unwrap_or(file_name)
            .to_string();
        let taken = |by: TargetTakenBy| {
            EngineError::Rejected(Rejection::SpecDraftTargetTaken {
                path: path.clone(),
                taken: by,
            })
        };
        let revising = new.target_kind == TargetKind::Revise;
        match (&opened.blob, revising) {
            (DocBlob::Missing, true) => {
                return Err(Rejection::FileNotFound { path: path.clone() }.into());
            }
            (DocBlob::NotRegular, true) => return Err(invalid("path", "invalid")),
            (DocBlob::TooLarge { .. }, true) => return Err(invalid("path", "too_large")),
            (DocBlob::Bytes { .. }, true) | (DocBlob::Missing, false) => {}
            (DocBlob::NotRegular, false) => {
                return Err(taken(TargetTakenBy::File {
                    blob: None,
                    commit: opened.commit.clone(),
                }));
            }
            (DocBlob::TooLarge { blob, .. } | DocBlob::Bytes { blob, .. }, false) => {
                return Err(taken(TargetTakenBy::File {
                    blob: Some(blob.clone()),
                    commit: opened.commit.clone(),
                }));
            }
        }
        self.refuse_taken_target(project_id, &path, &slug, revising)?;
        let mut total = 0u64;
        for (i, p) in new.context_paths.iter().enumerate() {
            match self
                .ports
                .docs
                .read_at(&repo, &opened.commit, p, MAX_CONTEXT_BYTES)
                .await?
            {
                Blob::Missing => return Err(not_found(format!("context_paths[{i}]"))),
                Blob::TooLarge { bytes } => total += bytes,
                Blob::Bytes(b) => total += b.len() as u64,
            }
        }
        if total > MAX_CONTEXT_BYTES {
            return Err(invalid("context_paths", "too_large"));
        }
        let new_incident = new.incident_id;
        let _guard = self.locked()?;
        self.refuse_taken_target(project_id, &path, &slug, revising)?;
        let now = self.now();
        let spec_draft_id = SpecDraftId(self.new_id());
        let event = core::request(NewDraft {
            spec_draft_id,
            project_id,
            path,
            slug,
            idea: new.idea,
            locale: new.locale,
            mode: new.mode,
            target_kind: new.target_kind,
            kind: new.kind,
            context_paths: new.context_paths,
            deployment_id: new.deployment_id,
            incident_id: new.incident_id,
        });
        let mut after = core::evolve(None, &event, now)
            .ok_or_else(|| EngineError::Internal("draft not requested".into()))?;
        after.draft.created_by = cause.actor.clone();
        let mut batch = Batch::new(now);
        self.observe_draft(&event, &after);
        self.push_event(&mut batch, &event, Some(project_id), cause, now);
        if let Some(action) = action {
            let reply = (action.respond)(Produced::Draft(&after));
            batch.action =
                Some(action.record(self, cause, Some(project_id), Some(spec_draft_id.0), reply));
        }
        batch.drafts.push(after.clone());
        if let Some(incident_id) = new_incident {
            self.link_incident(
                &mut batch,
                incident_id,
                saka_core::incident::Recovery::Draft(spec_draft_id),
                project_id,
                cause,
                now,
            )?;
        }
        self.commit(&batch)?;
        Ok(after)
    }

    fn refuse_taken_target(
        &self,
        project: ProjectId,
        path: &str,
        slug: &str,
        revising: bool,
    ) -> Result<()> {
        let (spec, by_slug, live) = self.store.read(|c| {
            Ok((
                read::spec_by_path(c, project, path)?,
                list::spec_by_slug(c, project, slug)?,
                rows::live_draft_at(c, project, path)?,
            ))
        })?;
        let by = if let Some(s) = spec.filter(|_| !revising) {
            TargetTakenBy::Spec { spec_id: s.id }
        } else if let Some(s) = by_slug.filter(|_| !revising) {
            TargetTakenBy::Slug { spec_id: s.id }
        } else if let Some(spec_draft_id) = live {
            TargetTakenBy::Draft { spec_draft_id }
        } else {
            return Ok(());
        };
        Err(Rejection::SpecDraftTargetTaken {
            path: path.to_string(),
            taken: by,
        }
        .into())
    }

    pub fn retry_spec_draft(
        &self,
        id: SpecDraftId,
        why: &str,
        note: Option<String>,
        cause: &Cause,
        action: Option<&Action<'_>>,
    ) -> Result<Vec<Event>> {
        let reason = reason(why)?;
        let note = match note {
            Some(n) => {
                let len = n.trim().chars().count();
                if len == 0 || len > NOTE_MAX {
                    return Err(invalid("note", "length"));
                }
                Some(n.trim().to_string())
            }
            None => None,
        };
        self.decide_draft(
            id,
            Some(DraftInput::Retry { reason, note }),
            Mode::Command,
            cause,
            Extra {
                action: action.map(|a| (a, Shows::Draft)),
                ..Extra::default()
            },
        )
    }

    pub fn message_spec_draft(
        &self,
        id: SpecDraftId,
        text: &str,
        cause: &Cause,
        action: Option<&Action<'_>>,
    ) -> Result<Vec<Event>> {
        let text = bounded(text, MESSAGE_MAX, "text")?;
        self.decide_draft(
            id,
            Some(DraftInput::Message { text }),
            Mode::Command,
            cause,
            Extra {
                action: action.map(|a| (a, Shows::Draft)),
                ..Extra::default()
            },
        )
    }

    pub async fn commit_spec_draft(
        &self,
        id: SpecDraftId,
        message: Option<&str>,
        cause: &Cause,
        action: Option<&Action<'_>>,
    ) -> Result<Vec<Event>> {
        let message = message
            .map(|m| bounded(m, COMMIT_MESSAGE_MAX, "message"))
            .transpose()?;
        let draft = self.load_draft(id)?;
        let check = if draft.draft.state == SpecDraftState::AwaitingOperator {
            self.commit_check(&draft).await?
        } else {
            CommitCheck::default()
        };
        self.decide_draft(
            id,
            Some(DraftInput::Commit { message, check }),
            Mode::Command,
            cause,
            Extra {
                action: action.map(|a| (a, Shows::Draft)),
                ..Extra::default()
            },
        )
    }

    async fn commit_check(&self, d: &Draft) -> Result<CommitCheck> {
        let Some(base_sha) = d.running_attempt().and_then(|a| a.base_sha.clone()) else {
            return Ok(CommitCheck::default());
        };
        let files = self.changed_docs(d, 0).await?;
        let (project, repo) = self.workspace_of(d.draft.project_id)?;
        if let Some(env) = self.live_env_ref(d)? {
            for file in &files {
                let read = self
                    .ports
                    .env
                    .read_worktree_file(&env, &file.path, MAX_DOC_BYTES)
                    .await?;
                if matches!(read, WorktreeFile::TooLarge { .. }) {
                    return Err(invalid("files", "too_large"));
                }
            }
        }
        let docs = &self.ports.docs;
        let base: BTreeMap<String, String> = docs
            .tree_at(&repo, &base_sha, &project.project_path)
            .await?
            .into_iter()
            .map(|e| (e.path, e.blob))
            .collect();
        let tip: BTreeMap<String, String> = docs
            .tree(
                &repo,
                &project.base_branch,
                &project.project_path,
                paths::MAX_TREE_FILES,
            )
            .await?
            .files
            .into_iter()
            .map(|e| (e.path, e.blob))
            .collect();
        let moved = files
            .iter()
            .filter(|f| base.get(&f.path) != tip.get(&f.path))
            .map(|f| f.path.clone())
            .collect();
        Ok(CommitCheck {
            changed: files.len(),
            moved,
        })
    }

    fn live_env_ref(&self, d: &Draft) -> Result<Option<EnvRef>> {
        let owner = owner_of(d);
        let Some(env) = d.ready_environment().filter(|e| Some(e.owner) == owner) else {
            return Ok(None);
        };
        let (_, repo) = self.workspace_of(d.draft.project_id)?;
        Ok(Some(self.draft_env_ref(repo, env)))
    }

    async fn changed_docs(&self, d: &Draft, max_diff: usize) -> Result<Vec<DraftChange>> {
        let Some(env) = self.live_env_ref(d)? else {
            return Ok(Vec::new());
        };
        let (project, _) = self.workspace_of(d.draft.project_id)?;
        let changes = self.ports.env.worktree_changes(&env, max_diff).await?;
        Ok(changes
            .into_iter()
            .filter(|c| paths::check_doc_path(&project.project_path, &c.path).is_ok())
            .map(|c| DraftChange {
                path: c.path,
                added: c.added,
                diff: c.diff,
                truncated: c.truncated,
            })
            .collect())
    }

    pub async fn spec_draft_changes(&self, id: SpecDraftId) -> Result<DraftChangesView> {
        let (draft, round) = self
            .store
            .read(|c| Ok((rows::load_draft(c, id)?, rows::last_round(c, id)?)))?;
        let draft = draft.ok_or(EngineError::Rejected(Rejection::NotFound))?;
        let files = if draft.draft.mode == DraftMode::Session {
            self.changed_docs(&draft, MAX_CHANGE_DIFF_BYTES).await?
        } else {
            Vec::new()
        };
        Ok(DraftChangesView {
            files,
            passed: round
                .as_ref()
                .is_some_and(|(_, r)| r.outcome == LintOutcome::Passed),
            findings: round.map(|(_, r)| r.issues).unwrap_or_default(),
        })
    }

    pub fn discard_spec_draft(
        &self,
        id: SpecDraftId,
        why: &str,
        cause: &Cause,
        action: Option<&Action<'_>>,
    ) -> Result<Vec<Event>> {
        let reason = reason(why)?;
        self.decide_draft(
            id,
            Some(DraftInput::Discard { reason }),
            Mode::Command,
            cause,
            Extra {
                action: action.map(|a| (a, Shows::Draft)),
                ..Extra::default()
            },
        )
    }

    pub fn cancel_spec_draft(
        &self,
        id: SpecDraftId,
        why: &str,
        cause: &Cause,
        action: Option<&Action<'_>>,
    ) -> Result<Vec<Event>> {
        let reason = reason(why)?;
        self.decide_draft(
            id,
            Some(DraftInput::Cancel { reason }),
            Mode::Command,
            cause,
            Extra {
                action: action.map(|a| (a, Shows::Draft)),
                ..Extra::default()
            },
        )
    }

    pub(crate) fn admit_spec_draft(&self, id: SpecDraftId, cause: &Cause) -> Result<Vec<Event>> {
        let attempt_id = SpecDraftAttemptId(self.new_id());
        self.decide_draft(
            id,
            Some(DraftInput::Admit {
                attempt_id,
                budget: self.cfg.draft.budget,
            }),
            Mode::Fact,
            cause,
            Extra::default(),
        )
    }

    fn draft_env_ref(&self, repo: RepoRef, env: &Environment) -> EnvRef {
        EnvRef {
            environment_id: env.id,
            repo,
            branch: None,
            root_path: env.root_path.clone(),
            worktree_path: env.worktree_path.clone(),
            vars: BTreeMap::from([("SAKA_ENVIRONMENT_ID".to_string(), env.id.to_string())]),
            path_prefix: Vec::new(),
            secrets: Vec::new(),
            extra_dirs: Vec::new(),
        }
    }

    fn workspace(&self, d: &Draft) -> std::result::Result<(Project, RepoRef), PortError> {
        self.workspace_of(d.draft.project_id).map_err(internal)
    }

    pub(crate) async fn execute_draft(
        &self,
        job: &saka_store::ClaimedJob,
        id: SpecDraftId,
        attempt: SpecDraftAttemptId,
    ) -> Execution {
        let Some(d) = self
            .store
            .read(|c| rows::load_draft(c, id))
            .map_err(|e| internal(e.into()))?
        else {
            return Ok(Effect::Skipped);
        };
        let p = &job.payload;
        let draft = |input| Effect::Draft {
            input,
            events: Vec::new(),
        };
        use saka_core::job::JobKind as K;
        match job.kind {
            K::EnvPrepare => self.draft_prepare(&d, attempt).await,
            K::SessionStart => self.draft_session_start(&d, attempt, job.id).await,
            K::SessionSendTurn => {
                self.draft_send_turn(
                    &d,
                    field(p, "session_id")?,
                    field(p, "turn")?,
                    field(p, "prompt")?,
                )
                .await
            }
            K::ScmPushDocsBranch | K::ScmOpenDocsChangeRequest | K::ScmMergeDocs => {
                Ok(Effect::Skipped)
            }
            K::SessionDeliverPermissionAnswer
            | K::SessionDeliverDecision
            | K::SessionInterrupt
            | K::SessionSteer
            | K::SessionReattach
            | K::SessionStop => Ok(self
                .session_job(job.kind, p, d.session.as_ref(), &d.permissions)
                .await?
                .effect(|i| draft(DraftInput::Session(i)))),
            K::DraftLint => {
                self.draft_lint(&d, attempt, field(p, "session_id")?, field(p, "turn")?)
                    .await
            }
            K::DraftCommit => {
                let message = p.get("message").and_then(Value::as_str).map(str::to_string);
                self.draft_commit(&d, attempt, message).await
            }
            K::EnvTeardown => self.draft_teardown(&d, field(p, "environment_id")?).await,
            K::EnvInspectWorktree
            | K::EnvRevert
            | K::EnvPrepareReview
            | K::ScmPostReview
            | K::ScmPushBranch
            | K::ScmOpenChangeRequest
            | K::ScmPollChangeRequest
            | K::ScmMerge
            | K::ScmIntegrate
            | K::ScmUpdateBranch
            | K::ScmRerunChecks
            | K::ScmPostComment
            | K::EnvVerifyMerged
            | K::ScmWebhookPrune
            | K::WorkspaceReloadConfig
            | K::IncidentDraft
            | K::ScmScanChanges
            | K::ScmScanIssues
            | K::IntakeIssueRender
            | K::ScmIssueComment
            | K::ScmIssueClose
            | K::DeployRun
            | K::DeployVerify
            | K::ScmUserLinkPoll
            | K::ScmUserLinkRefresh
            | K::GateVerify
            | K::GateReproduce
            | K::EnvResetBranch
            | K::SessionDeliverRegression
            | K::ScmReportStatus
            | K::ScmPollCommit
            | K::ScmDeleteBranch
            | K::HookRun => Ok(Effect::Skipped),
        }
    }

    async fn draft_prepare(&self, d: &Draft, attempt_id: SpecDraftAttemptId) -> Execution {
        let Some(attempt) = d.running_attempt().filter(|a| a.id == attempt_id) else {
            return Ok(Effect::Skipped);
        };
        let owner = owner_of(d);
        let ready = d
            .ready_environment()
            .is_some_and(|e| Some(e.owner) == owner);
        if d.draft.state != SpecDraftState::Preparing || ready {
            return Ok(Effect::Skipped);
        }
        let environment_id = EnvironmentId(attempt_id.0);
        let (project, repo) = self.workspace(d)?;
        let prepared = self
            .ports
            .env
            .prepare(&PrepareRequest {
                environment_id,
                repo: repo.clone(),
                checkout: Checkout::Detached,
                base_branch: project.base_branch.clone(),
            })
            .await?;
        let env = EnvRef {
            environment_id,
            repo: repo.clone(),
            branch: None,
            root_path: prepared.root_path.clone(),
            worktree_path: prepared.worktree_path.clone(),
            vars: BTreeMap::new(),
            path_prefix: Vec::new(),
            secrets: Vec::new(),
            extra_dirs: Vec::new(),
        };
        let code = self.prepare_code(d, &project, environment_id).await?;
        let previous = self.previous_attempt(d, attempt.attempt)?;
        if let Some((_, Some(content))) = &previous {
            self.ports
                .env
                .write_worktree_file(&env, &d.draft.path, content)
                .await?;
        }
        let previous = previous.map(|(p, _)| p);
        let rendered = self
            .architect_packet(d, &project, &repo, &prepared.base_sha, &code, previous)
            .await?;
        let packet = match rendered {
            Ok(packet) => packet,
            Err(missing) => {
                self.teardown_code(&project, environment_id).await?;
                self.ports.env.teardown(&env).await?;
                return Ok(Effect::Draft {
                    input: DraftInput::EnvironmentFailed {
                        code: DraftFailureCode::ContextUnavailable,
                        params: json!({ "path": missing }),
                    },
                    events: Vec::new(),
                });
            }
        };
        tracing::info!(
            spec_draft_id = %d.draft.id,
            attempt = attempt.attempt,
            bytes = packet.text.len(),
            sha256 = %packet.sha256,
            documents = packet.documents.len(),
            "spec_draft.packet"
        );
        Ok(Effect::Draft {
            input: DraftInput::EnvironmentPrepared {
                environment_id,
                repo_id: repo.repo_id,
                root_path: prepared.root_path,
                worktree_path: prepared.worktree_path,
                base_sha: prepared.base_sha,
                packet: PacketFacts {
                    text: packet.text,
                    sha256: packet.sha256,
                    documents: packet.documents,
                    code_commits: code.commits(),
                },
            },
            events: Vec::new(),
        })
    }

    fn previous_attempt(
        &self,
        d: &Draft,
        attempt: u32,
    ) -> std::result::Result<Option<Previous>, PortError> {
        if attempt <= 1 {
            return Ok(None);
        }
        let id = d.draft.id;
        let found = self
            .store
            .read(|c| {
                let Some(prev) = rows::previous_attempt(c, id, attempt - 1)? else {
                    return Ok(None);
                };
                let last = rows::last_round_of(c, prev.id)?;
                Ok(Some((prev, last)))
            })
            .map_err(|e| internal(e.into()))?;
        Ok(found.map(|(prev, last)| {
            let content = last.as_ref().and_then(|r| r.content.clone());
            let previous = PreviousAttempt {
                attempt: prev.attempt,
                code: prev
                    .failure
                    .as_ref()
                    .map_or_else(|| "cancelled".to_string(), |f| f.code.clone()),
                seeded: content.is_some(),
                issues: last.map(|r| r.issues).unwrap_or_default(),
            };
            (previous, content)
        }))
    }

    async fn architect_packet(
        &self,
        d: &Draft,
        project: &Project,
        repo: &RepoRef,
        base_sha: &str,
        code: &CodeSet,
        previous: Option<PreviousAttempt>,
    ) -> std::result::Result<std::result::Result<ArchitectPacket, String>, PortError> {
        let docs = &self.ports.docs;
        let mut selected = Vec::new();
        let mut unreadable = Vec::new();
        for path in &d.draft.context_paths {
            match docs
                .read_at(repo, base_sha, path, MAX_CONTEXT_BYTES)
                .await?
            {
                Blob::Missing => return Ok(Err(path.clone())),
                Blob::TooLarge { .. } => unreadable.push(path.clone()),
                Blob::Bytes(bytes) => selected.push((path.clone(), bytes)),
            }
        }
        let mut always = Vec::new();
        for path in docs
            .find_always(repo, base_sha, &project.project_path)
            .await?
        {
            if path == d.draft.path || d.draft.context_paths.contains(&path) {
                continue;
            }
            match docs
                .read_at(repo, base_sha, &path, MAX_CONTEXT_BYTES)
                .await?
            {
                Blob::Bytes(bytes) if saka_spec::context::is_always(&bytes) => {
                    always.push((path, bytes));
                }
                Blob::TooLarge { .. } => unreadable.push(path),
                Blob::Bytes(_) | Blob::Missing => {}
            }
        }
        let index = docs
            .tree_at(repo, base_sha, &project.project_path)
            .await?
            .into_iter()
            .map(|e| e.path)
            .collect();
        Ok(Ok(packet::render_architect(&ArchitectInput {
            project_name: project.name.clone(),
            project_path: project.project_path.clone(),
            base_sha: base_sha.to_string(),
            locale: d.draft.locale,
            mode: d.draft.mode,
            target_kind: d.draft.target_kind,
            kind: d.draft.kind,
            idea: d.draft.idea.clone(),
            target: d.draft.path.clone(),
            selected,
            always,
            unreadable,
            index,
            code: code.entries.clone(),
            code_skipped: code.skipped.clone(),
            previous,
            note: d.running_attempt().and_then(|a| a.note.clone()),
        })))
    }

    async fn draft_session_start(
        &self,
        d: &Draft,
        attempt_id: SpecDraftAttemptId,
        job: uuid::Uuid,
    ) -> Execution {
        let Some(owner) = owner_of(d).filter(|o| o.attempt_id() == Some(attempt_id)) else {
            return Ok(Effect::Skipped);
        };
        let Some(env) = d.ready_environment().filter(|e| e.owner == owner) else {
            return Ok(Effect::Skipped);
        };
        if d.draft.state != SpecDraftState::Drafting || d.live_session().is_some() {
            return Ok(Effect::Skipped);
        }
        let session_id = match &d.session {
            Some(previous) if previous.owner == owner => SessionId(job),
            _ => SessionId(attempt_id.0),
        };
        let Some(view) = core::run_view(d) else {
            return Ok(Effect::Skipped);
        };
        let (project, repo) = self.workspace(d)?;
        let mut environment = self.draft_env_ref(repo, env);
        environment.extra_dirs = self.code_extra_dirs(d);
        let launched = self
            .ports
            .agents
            .start(&SessionLaunch {
                session_id,
                owner,
                environment,
                resume_cli_session_id: saka_core::resume::plan(&view)
                    .and_then(|p| p.cli_session_id),
                profile: LaunchProfile::Architect {
                    cwd: cwd_of(&project.project_path),
                },
                model: self.cfg.architect_model.clone(),
                author: self.cfg.git_identity.clone(),
                regression_check: false,
            })
            .await?;
        Ok(Effect::Draft {
            input: DraftInput::Session(saka_core::session::SessionInput::Launched {
                session_id,
                cli: launched.cli,
                cli_version: launched.cli_version,
                model: launched.model,
                account: launched.account,
                runner_unit: launched.runner_unit,
                socket_path: launched.socket_path,
                log_path: launched.log_path,
                author: saka_core::authorship::SessionAuthor::saka(),
            }),
            events: Vec::new(),
        })
    }

    async fn draft_send_turn(
        &self,
        d: &Draft,
        session: SessionId,
        turn: u32,
        prompt: Prompt,
    ) -> Execution {
        let Some(target) = session_ref(d.session.as_ref(), session) else {
            return Ok(Effect::Skipped);
        };
        let Some(attempt) = d.running_attempt() else {
            return Ok(Effect::Skipped);
        };
        let path = &d.draft.path;
        let body = match &prompt {
            Prompt::DraftPacket => self
                .store
                .read(|c| rows::packet(c, attempt.id))
                .map_err(|e| internal(e.into()))?
                .ok_or_else(|| PortError::permanent("packet_missing", json!({})))?,
            Prompt::DraftLint { round, max_rounds } => {
                let found = self
                    .store
                    .read(|c| rows::lint_round(c, attempt.id, *round))
                    .map_err(|e| internal(e.into()))?
                    .ok_or_else(|| PortError::permanent("lint_round_missing", json!({})))?;
                let blocking = packet::blocking_issues(&found.issues);
                format!(
                    "Saka checked {path} after your last turn (check {round} of {max_rounds}). It does not pass yet:\n{}Fix every issue in {path}, then end your turn. Unanswered decisions (decision_unanswered) are expected; do not answer them.",
                    packet::issue_list(&blocking)
                )
            }
            Prompt::DraftMissing { round, max_rounds } => format!(
                "Saka found no file at {path} after your last turn (check {round} of {max_rounds}). Write the complete spec to {path}, then end your turn."
            ),
            Prompt::Resume { cause, .. } => saka_core::resume::text(*cause),
            Prompt::Steer { text } => text.clone(),
            Prompt::Decision { decision_id } => d
                .decisions
                .iter()
                .find(|r| r.id == *decision_id)
                .map(saka_core::decision::answer_text)
                .ok_or_else(|| PortError::permanent("decision_missing", json!({})))?,
            Prompt::WorkPacket { .. }
            | Prompt::Uncommitted { .. }
            | Prompt::OutOfScope { .. }
            | Prompt::DocsUncommitted { .. }
            | Prompt::DocsOutOfScope { .. }
            | Prompt::ReviewPacket
            | Prompt::Regression { .. }
            | Prompt::VerdictFormat { .. } => {
                return Err(PortError::permanent(
                    "prompt_invalid",
                    json!({ "prompt": "task" }),
                ));
            }
        };
        self.ports.agents.send_turn(&target, turn, &body).await?;
        Ok(Effect::Draft {
            input: DraftInput::Session(saka_core::session::SessionInput::TurnAccepted { turn }),
            events: Vec::new(),
        })
    }

    async fn draft_lint(
        &self,
        d: &Draft,
        attempt_id: SpecDraftAttemptId,
        session_id: SessionId,
        turn: u32,
    ) -> Execution {
        let Some(attempt) = d.running_attempt().filter(|a| a.id == attempt_id) else {
            return Ok(Effect::Skipped);
        };
        let fresh = d.draft.state == SpecDraftState::Drafting
            && d.live_session()
                .is_some_and(|s| s.id == session_id && s.turns == turn && core::turn_settled(d, s));
        let seen = d
            .last_round
            .is_some_and(|m| m.session_id == session_id && m.turn >= turn);
        let owner = owner_of(d);
        let env = d.ready_environment().filter(|e| Some(e.owner) == owner);
        let (Some(env), Some(base_sha)) = (env, attempt.base_sha.clone()) else {
            return Ok(Effect::Skipped);
        };
        if !fresh || seen {
            return Ok(Effect::Skipped);
        }
        let (project, repo) = self.workspace(d)?;
        let path = d.draft.path.clone();
        let read = self
            .ports
            .env
            .read_worktree_file(
                &self.draft_env_ref(repo.clone(), env),
                &path,
                MAX_ROUND_BYTES + 1,
            )
            .await?;
        let failed = |issue: LintIssue, bytes: u64| LintRound {
            spec_draft_id: d.draft.id,
            attempt_id,
            round: 0,
            session_id,
            turn,
            outcome: LintOutcome::Failed,
            bytes: Some(bytes),
            content: None,
            content_hash: None,
            parser_version: PARSER_VERSION,
            issues: vec![issue_value(&issue)],
            decisions_open: 0,
            created_at: self.now(),
        };
        let round = match read {
            WorktreeFile::Missing => LintRound {
                spec_draft_id: d.draft.id,
                attempt_id,
                round: 0,
                session_id,
                turn,
                outcome: LintOutcome::Missing,
                bytes: None,
                content: None,
                content_hash: None,
                parser_version: PARSER_VERSION,
                issues: Vec::new(),
                decisions_open: 0,
                created_at: self.now(),
            },
            WorktreeFile::NotRegular => failed(LintIssue::new(LintCode::NotASpec, 1), 0),
            WorktreeFile::TooLarge { bytes } => failed(too_large(bytes), bytes),
            WorktreeFile::Bytes(bytes) if bytes.len() as u64 > MAX_ROUND_BYTES => {
                let size = bytes.len() as u64;
                failed(too_large(size), size)
            }
            WorktreeFile::Bytes(bytes) => {
                let file_name = path.rsplit('/').next().unwrap_or(&path);
                let parsed = saka_spec::parse(file_name, &bytes);
                let context = self
                    .resolve_context(&repo, &project.project_path, &path, &base_sha, &parsed)
                    .await
                    .map_err(|e| match e {
                        EngineError::Port(p) => p,
                        other => internal(other),
                    })?;
                let mut issues = parsed.issues.clone();
                issues.extend(context.issues.iter().cloned());
                issues.sort_by_key(|i| i.line);
                let open = issues
                    .iter()
                    .filter(|i| i.code == LintCode::DecisionUnanswered)
                    .count();
                let passed = !issues
                    .iter()
                    .any(|i| i.is_error() && i.code != LintCode::DecisionUnanswered);
                LintRound {
                    spec_draft_id: d.draft.id,
                    attempt_id,
                    round: 0,
                    session_id,
                    turn,
                    outcome: if passed {
                        LintOutcome::Passed
                    } else {
                        LintOutcome::Failed
                    },
                    bytes: Some(bytes.len() as u64),
                    content_hash: Some(content_hash(&bytes)),
                    content: Some(bytes),
                    parser_version: PARSER_VERSION,
                    issues: issues.iter().map(issue_value).collect(),
                    decisions_open: u32::try_from(open).unwrap_or(u32::MAX),
                    created_at: self.now(),
                }
            }
        };
        Ok(Effect::Draft {
            input: DraftInput::Linted(round),
            events: Vec::new(),
        })
    }

    async fn draft_commit(
        &self,
        d: &Draft,
        attempt_id: SpecDraftAttemptId,
        message: Option<String>,
    ) -> Execution {
        let current = d.running_attempt().filter(|a| a.id == attempt_id);
        let Some(attempt) = current.filter(|_| d.draft.state == SpecDraftState::Committing) else {
            return Ok(Effect::Skipped);
        };
        let round = self
            .store
            .read(|c| rows::last_round_of(c, attempt_id))
            .map_err(|e| internal(e.into()))?
            .filter(|r| r.outcome == LintOutcome::Passed);
        let Some(bytes) = round.and_then(|r| r.content) else {
            return Err(PortError::permanent("lint_round_missing", json!({})));
        };
        let (project, repo) = self.workspace(d)?;
        let path = d.draft.path.clone();
        let failed = |code: DraftFailureCode, params: Value, commit: Option<String>| {
            Ok(Effect::Draft {
                input: DraftInput::CommitFailed {
                    code,
                    params,
                    commit,
                },
                events: Vec::new(),
            })
        };
        let mut files = vec![(path.clone(), bytes.clone())];
        if d.draft.mode == DraftMode::Session
            && let Some(env) = self.live_env_ref(d).map_err(internal)?
        {
            for change in self.changed_docs(d, 0).await.map_err(internal)? {
                if change.path == path {
                    continue;
                }
                match self
                    .ports
                    .env
                    .read_worktree_file(&env, &change.path, paths::MAX_DOC_BYTES)
                    .await?
                {
                    WorktreeFile::Bytes(b) => files.push((change.path, b)),
                    WorktreeFile::Missing => {}
                    WorktreeFile::NotRegular | WorktreeFile::TooLarge { .. } => {
                        return Err(PortError::permanent(
                            "worktree_file_unusable",
                            json!({ "path": change.path }),
                        ));
                    }
                }
            }
        }
        let at_base: BTreeMap<String, String> = match &attempt.base_sha {
            Some(base) if files.len() > 1 || d.draft.target_kind == TargetKind::Revise => self
                .ports
                .docs
                .tree_at(&repo, base, &project.project_path)
                .await?
                .into_iter()
                .map(|e| (e.path, e.blob))
                .collect(),
            _ => BTreeMap::new(),
        };
        let expect = |file_path: &str| match at_base.get(file_path) {
            Some(blob) => DocExpect::Blob(blob.clone()),
            None => DocExpect::Absent,
        };
        let request = DocFilesCommit {
            repo: repo.clone(),
            branch: project.base_branch.clone(),
            files: files
                .iter()
                .map(|(file_path, content)| DocFileWrite {
                    path: file_path.clone(),
                    bytes: content.clone(),
                    expect: expect(file_path),
                })
                .collect(),
            message,
            author: self.cfg.git_identity.clone(),
            at: self.now(),
        };
        let (commit, events) = match self.ports.docs.commit_files(&request).await? {
            DocFilesWrite::Committed {
                commit,
                parent,
                files: committed,
                ..
            } => {
                tracing::info!(
                    spec_draft_id = %d.draft.id,
                    attempt = d.draft.attempt,
                    %commit,
                    files = committed.len(),
                    "spec_draft.committed"
                );
                let sizes: BTreeMap<&str, u64> = files
                    .iter()
                    .map(|(file_path, content)| (file_path.as_str(), content.len() as u64))
                    .collect();
                let events = committed
                    .into_iter()
                    .map(|f| Event::DocCommitted {
                        project_id: project.id,
                        bytes: sizes.get(f.path.as_str()).copied().unwrap_or_default(),
                        path: f.path,
                        commit: commit.clone(),
                        parent: parent.clone(),
                        blob: f.blob,
                        created: f.created,
                    })
                    .collect::<Vec<_>>();
                (commit, events)
            }
            DocFilesWrite::Unchanged { commit } => (commit, Vec::new()),
            DocFilesWrite::Stale { commit, stale } => {
                let target = stale.iter().find(|s| s.path == path);
                return match target {
                    Some(t) if d.draft.target_kind == TargetKind::New => failed(
                        DraftFailureCode::TargetExists,
                        json!({ "blob": t.blob, "commit": commit }),
                        None,
                    ),
                    _ => failed(
                        DraftFailureCode::BaseMoved,
                        json!({ "paths": stale.iter().map(|s| s.path.clone()).collect::<Vec<_>>() }),
                        None,
                    ),
                };
            }
            DocFilesWrite::PathConflict { path: conflict } => {
                return failed(
                    DraftFailureCode::TargetPathConflict,
                    json!({ "path": conflict }),
                    None,
                );
            }
            DocFilesWrite::NotRegular => {
                return failed(
                    DraftFailureCode::TargetPathConflict,
                    json!({ "path": path }),
                    None,
                );
            }
            DocFilesWrite::Rejected => {
                return failed(
                    DraftFailureCode::PushRejected,
                    json!({ "branch": project.base_branch }),
                    None,
                );
            }
        };
        let cause = self.cause(ARCHITECT_ACTOR);
        for (file_path, content) in files.iter().filter(|(file_path, _)| *file_path != path) {
            self.register_saved(&project, &repo, file_path, &commit, content, &cause)
                .await
                .map_err(|e| match e {
                    EngineError::Port(p) => p,
                    other => internal(other),
                })?;
        }
        let from_issue = self
            .store
            .read(|c| saka_store::issue_intake::issue_intake_of_draft(c, d.draft.id))
            .map_err(|e| internal(e.into()))?
            .is_some();
        let file = SpecFile {
            path: &path,
            source_commit: &commit,
            bytes: &bytes,
            origin: if from_issue || d.draft.incident_id.is_some() {
                SpecOrigin::Intake
            } else {
                SpecOrigin::Architect
            },
        };
        let registered = self.register_at(&project, &repo, file, &cause, None).await;
        let problem = match registered {
            Ok((view, _)) => {
                return Ok(Effect::Draft {
                    input: DraftInput::Committed {
                        commit,
                        spec_id: view.spec.id,
                        revision_id: view.revision.id,
                    },
                    events,
                });
            }
            Err(EngineError::Rejected(r)) => json!({ "code": r.code(), "params": r.params() }),
            Err(EngineError::Invalid { field, code }) => json!({
                "code": "validation_failed",
                "params": { "errors": [{ "field": field, "code": code }] },
            }),
            Err(EngineError::Port(e)) if !e.retryable => {
                json!({ "code": e.code, "params": e.params })
            }
            Err(EngineError::Port(e)) => return Err(e),
            Err(other) => return Err(internal(other)),
        };
        Ok(Effect::Draft {
            input: DraftInput::CommitFailed {
                code: DraftFailureCode::RegisterFailed,
                params: problem,
                commit: Some(commit),
            },
            events,
        })
    }

    async fn draft_teardown(&self, d: &Draft, id: EnvironmentId) -> Execution {
        let Some(env) = d
            .environment_by_id(id)
            .filter(|e| e.state != saka_core::vocab::EnvironmentState::TornDown)
        else {
            return Ok(Effect::Skipped);
        };
        let (project, repo) = self.workspace(d)?;
        let code = self.teardown_code(&project, id).await;
        self.ports
            .env
            .teardown(&self.draft_env_ref(repo, env))
            .await?;
        code?;
        Ok(Effect::Draft {
            input: DraftInput::EnvironmentTornDown { environment_id: id },
            events: Vec::new(),
        })
    }

    pub(crate) fn reconcile_drafts(
        &self,
        quotas: &[saka_core::model::AccountQuota],
        cause: &Cause,
        batch: &mut Batch,
    ) -> Result<usize> {
        let now = self.now();
        let drafts = self.store.read(|c| rows::load_drafts(c, DraftSet::Live))?;
        let mut decisions = 0;
        for action in core::reconcile(&drafts, quotas, &self.cfg.draft, now) {
            match action {
                core::DraftAction::Admit(id) => {
                    if !self.admit_spec_draft(id, cause)?.is_empty() {
                        decisions += 1;
                    }
                }
                core::DraftAction::Tick(id) => {
                    let events = self.decide_draft(
                        id,
                        Some(DraftInput::Tick),
                        Mode::Fact,
                        cause,
                        Extra::default(),
                    )?;
                    if !events.is_empty() {
                        decisions += 1;
                    }
                }
                core::DraftAction::Enqueue(job) => {
                    let project = saka_core::job::spec_draft_of(&job.payload)
                        .and_then(|id| drafts.iter().find(|d| d.draft.id == id))
                        .map(|d| d.draft.project_id);
                    self.push_job(batch, job, project, cause);
                }
            }
        }
        for job in self.monitor_sessions(drafts.iter().filter_map(Draft::live_session)) {
            let project = saka_core::job::spec_draft_of(&job.payload)
                .and_then(|id| drafts.iter().find(|d| d.draft.id == id))
                .map(|d| d.draft.project_id);
            self.push_job(batch, job, project, cause);
        }
        Ok(decisions)
    }

    pub(crate) fn recover_drafts(&self, cause: &Cause, batch: &mut Batch) -> Result<()> {
        let drafts = self.store.read(|c| rows::load_drafts(c, DraftSet::Live))?;
        for job in self.monitor_sessions(drafts.iter().filter_map(Draft::live_session)) {
            let project = saka_core::job::spec_draft_of(&job.payload)
                .and_then(|id| drafts.iter().find(|d| d.draft.id == id))
                .map(|d| d.draft.project_id);
            self.push_job(batch, job, project, cause);
        }
        Ok(())
    }
}

fn too_large(bytes: u64) -> LintIssue {
    LintIssue::new(LintCode::FileTooLarge, 1)
        .with("bytes", bytes)
        .with("max_bytes", MAX_ROUND_BYTES)
}
