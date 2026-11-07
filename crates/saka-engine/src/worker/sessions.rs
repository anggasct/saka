use std::collections::BTreeMap;

use saka_core::authorship::{self, CredentialBlock, ResolvedAuthor, SessionAuthor, UserLink};
use saka_core::event::Prompt;
use saka_core::ids::{SessionId, StageRunId};
use saka_core::model::{Environment, Task};
use saka_core::rejection::Rejection;
use saka_core::session::SessionInput;
use saka_core::task::{DocsInspection, TaskInput};
use saka_core::vocab::{
    AuthorshipMode, ChangeState, DocsMode, RefreshReason, ScopeStage, Stage, StagePhase,
    UserLinkCause, UserTokenCause,
};
use saka_ports::{ChangeCredential, EnvRef, LaunchProfile, PortError, SessionLaunch};

use super::{Effect, Execution, OpenedBy, Opener, Refusal, internal, session_ref};
use crate::accounts::LinkCredential;
use crate::{Engine, EngineError};

impl Engine {
    pub(super) async fn session_start(
        &self,
        task: &Task,
        run: StageRunId,
        job: uuid::Uuid,
    ) -> Execution {
        let current = task.stage_run.as_ref().is_some_and(|r| r.id == run);
        let Some(env) = task.ready_environment().filter(|_| current) else {
            return Ok(Effect::Skipped);
        };
        let reviewing = task.is_at(Stage::Review, StagePhase::RunningSession);
        if !(reviewing || task.is_writing_at(StagePhase::RunningSession))
            || task.live_session().is_some()
        {
            return Ok(Effect::Skipped);
        }
        let session_id = match &task.session {
            Some(previous) if previous.owner.stage_run_id() == Some(run) => SessionId(job),
            _ => SessionId(run.0),
        };
        let Some(view) = saka_core::task::run_view(task) else {
            return Ok(Effect::Skipped);
        };
        let (profile, environment, resolved) = if reviewing {
            let bare = EnvRef {
                environment_id: env.id,
                repo: self.repo_ref(task)?,
                branch: None,
                root_path: env.root_path.clone(),
                worktree_path: env.worktree_path.clone(),
                vars: BTreeMap::new(),
                path_prefix: Vec::new(),
                secrets: Vec::new(),
                extra_dirs: Vec::new(),
            };
            let bot = ResolvedAuthor {
                identity: self.cfg.git_identity.clone(),
                author: SessionAuthor::saka(),
            };
            (LaunchProfile::Reviewer, bare, bot)
        } else {
            let resolved = if task.external.is_some() {
                ResolvedAuthor {
                    identity: self.cfg.git_identity.clone(),
                    author: SessionAuthor::saka(),
                }
            } else {
                match self.session_author(task)? {
                    Ok(resolved) => resolved,
                    Err(block) => {
                        return Ok(Effect::Fact(TaskInput::CredentialBlocked {
                            target: saka_core::vocab::ChangeRole::Code,
                            block,
                        }));
                    }
                }
            };
            let environment = self
                .env_ref_with_secrets(task, env)
                .map_err(Refusal::into_error)?;
            (LaunchProfile::Task, environment, resolved)
        };
        self.align_branch(task, env).await?;
        let launched = self
            .ports
            .agents
            .start(&SessionLaunch {
                session_id,
                owner: view.owner,
                environment,
                resume_cli_session_id: saka_core::resume::plan(&view)
                    .and_then(|p| p.cli_session_id),
                profile,
                model: None,
                author: resolved.identity,
            })
            .await?;
        Ok(Effect::Fact(TaskInput::Session(SessionInput::Launched {
            session_id,
            cli: launched.cli,
            cli_version: launched.cli_version,
            model: launched.model,
            account: launched.account,
            runner_unit: launched.runner_unit,
            socket_path: launched.socket_path,
            log_path: launched.log_path,
            author: resolved.author,
        })))
    }

    pub(super) async fn align_branch(
        &self,
        task: &Task,
        env: &Environment,
    ) -> std::result::Result<(), PortError> {
        let Some(cr) = task
            .change_request
            .as_ref()
            .filter(|c| c.state == ChangeState::Open)
        else {
            return Ok(());
        };
        let env_ref = self.env_ref(task, env)?;
        let base = env.base_sha.clone().unwrap_or_default();
        let state = self.ports.env.inspect(&env_ref, &base).await?;
        if state.dirty || state.head_sha == cr.head_sha {
            return Ok(());
        }
        let on_remote = self
            .ports
            .env
            .contains_commit(&self.repo_ref(task)?, &state.head_sha, &task.item.branch)
            .await?;
        if on_remote.as_deref() == Some(cr.head_sha.as_str()) {
            self.ports.env.check_out(&env_ref, &cr.head_sha).await?;
        }
        Ok(())
    }

    pub(super) fn session_author(
        &self,
        task: &Task,
    ) -> std::result::Result<std::result::Result<ResolvedAuthor, CredentialBlock>, PortError> {
        let mode = self
            .authorship_mode(task.item.project_id)
            .map_err(internal)?;
        match mode {
            AuthorshipMode::Bot => Ok(Ok(ResolvedAuthor {
                identity: self.cfg.git_identity.clone(),
                author: SessionAuthor::saka(),
            })),
            AuthorshipMode::User => {
                let integration = self.repo_ref(task)?.integration_id;
                let link = self.operator_link(&integration)?;
                Ok(authorship::user_link_author(&integration, link.as_ref()))
            }
            AuthorshipMode::UserPat => {
                let integration = self.repo_ref(task)?.integration_id;
                let current = self
                    .project_authorship_now(task.item.project_id)
                    .map_err(internal)?;
                Ok(authorship::user_token_author(
                    current.as_ref(),
                    &integration,
                    self.now(),
                ))
            }
        }
    }

    pub(super) fn operator_link(
        &self,
        integration: &str,
    ) -> std::result::Result<Option<UserLink>, PortError> {
        self.store
            .read(|c| saka_store::user_link::user_link(c, crate::OPERATOR, integration))
            .map(|row| row.map(|r| r.link))
            .map_err(|e| internal(e.into()))
    }

    // The credential is the project's mode at the moment the change opens; a refresh that is due
    // happens first and is committed before the token is used.
    pub(super) async fn change_credential(
        &self,
        task: &Task,
        integration: &str,
    ) -> std::result::Result<std::result::Result<Opener, CredentialBlock>, PortError> {
        let mode = self
            .authorship_mode(task.item.project_id)
            .map_err(internal)?;
        let blocked = |cause: UserLinkCause| CredentialBlock::LinkRequired {
            integration_id: integration.to_string(),
            cause,
            phase: StagePhase::OpeningChange,
        };
        match mode {
            AuthorshipMode::Bot => Ok(Ok(Opener {
                credential: ChangeCredential::App,
                by: OpenedBy::App,
            })),
            AuthorshipMode::User => {
                let Some(link) = self.operator_link(integration)? else {
                    return Ok(Err(blocked(UserLinkCause::Missing)));
                };
                match self.refresh_user_link(link.id, RefreshReason::Use).await {
                    Ok(LinkCredential::Ready(token)) => Ok(Ok(Opener {
                        credential: ChangeCredential::User(token),
                        by: OpenedBy::Link(link),
                    })),
                    Ok(LinkCredential::Relink(cause)) => Ok(Err(blocked(cause.into()))),
                    Err(EngineError::Rejected(Rejection::UserLinkNotFound)) => {
                        Ok(Err(blocked(UserLinkCause::Missing)))
                    }
                    Err(EngineError::Port(e)) => Err(e),
                    Err(e) => Err(internal(e)),
                }
            }
            AuthorshipMode::UserPat => self.token_credential(task, integration),
        }
    }

    pub(super) fn token_credential(
        &self,
        task: &Task,
        integration: &str,
    ) -> std::result::Result<std::result::Result<Opener, CredentialBlock>, PortError> {
        let project = task.item.project_id;
        let phase = StagePhase::OpeningChange;
        let current = self.project_authorship_now(project).map_err(internal)?;
        if let Err(block) =
            authorship::usable_token(current.as_ref(), integration, phase, self.now())
        {
            return Ok(Err(block));
        }
        let rejected = CredentialBlock::TokenInvalid {
            cause: UserTokenCause::Rejected,
            phase,
        };
        let Some(opened) = self.open_user_token(project).map_err(internal)? else {
            return Ok(Err(rejected));
        };
        if opened.token.integration_id != integration {
            return Ok(Err(CredentialBlock::TokenInvalid {
                cause: UserTokenCause::Missing,
                phase,
            }));
        }
        Ok(Ok(Opener {
            credential: ChangeCredential::User(opened.value),
            by: OpenedBy::Token {
                set_at: opened.token.set_at,
                login: opened.token.identity.login,
            },
        }))
    }

    pub(super) async fn send_turn(
        &self,
        task: &Task,
        session: SessionId,
        turn: u32,
        prompt: Prompt,
    ) -> Execution {
        let Some(session) = session_ref(task.session.as_ref(), session) else {
            return Ok(Effect::Skipped);
        };
        let body = match &prompt {
            Prompt::WorkPacket { retry_of } if task.external.is_some() => {
                let retry = retry_of.as_ref().and_then(crate::text::retry_text);
                let title = task.external.as_ref().map_or("", |e| e.title.as_str());
                let text = crate::fix_packet::render(title, &task.item.branch, retry.as_deref());
                tracing::info!(
                    work_item_id = %task.item.id,
                    session_id = %session.session_id,
                    bytes = text.len(),
                    "fix.packet"
                );
                text
            }
            Prompt::WorkPacket { retry_of } => {
                let retry = retry_of.as_ref().and_then(crate::text::retry_text);
                let packet = self.packet_of(task, retry).map_err(internal)?;
                let bytes = packet.text.len() as u64;
                self.counters.work_packet(bytes);
                tracing::info!(
                    work_item_id = %task.item.id,
                    session_id = %session.session_id,
                    bytes,
                    sha256 = %packet.sha256,
                    documents = packet.documents.len(),
                    "session.work_packet"
                );
                packet.text
            }
            Prompt::ReviewPacket => self.review_packet_text(task).await?,
            other => self.turn_text(task, other).map_err(internal)?,
        };
        self.ports.agents.send_turn(&session, turn, &body).await?;
        Ok(Effect::Fact(TaskInput::Session(
            SessionInput::TurnAccepted { turn },
        )))
    }

    pub(super) async fn inspect(&self, task: &Task, session: SessionId, turn: u32) -> Execution {
        let (Some(_), Some(env)) = (
            session_ref(task.session.as_ref(), session),
            task.ready_environment(),
        ) else {
            return Ok(Effect::Skipped);
        };
        let base = env.base_sha.clone().unwrap_or_default();
        let state = self
            .ports
            .env
            .inspect(&self.env_ref(task, env)?, &base)
            .await?;
        let outside = if !state.dirty && state.commits_ahead > 0 {
            self.scope_violation(task, &base, &state.head_sha, false, ScopeStage::Dev)
                .await?
        } else {
            None
        };
        let docs = match env.docs.as_ref().filter(|d| d.mode == DocsMode::Writable) {
            None => None,
            Some(d) => {
                let state = self.ports.env.inspect_docs(env.id, &d.base_sha).await?;
                let outside = if !state.dirty && state.commits_ahead > 0 {
                    self.docs_violation(task, d, &d.commit, &state.head_sha, false, ScopeStage::Dev)
                        .await?
                } else {
                    None
                };
                Some(DocsInspection {
                    dirty: state.dirty,
                    commits_ahead: state.commits_ahead,
                    head_sha: state.head_sha,
                    outside,
                })
            }
        };
        Ok(Effect::Fact(TaskInput::WorktreeInspected {
            docs,
            turn,
            dirty: state.dirty,
            commits_ahead: state.commits_ahead,
            head_sha: state.head_sha,
            outside,
        }))
    }
}
