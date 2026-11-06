use saka_core::ids::{EnvironmentId, StageRunId};
use saka_core::model::Task;
use saka_core::review;
use saka_core::task::TaskInput;
use saka_core::vocab::{ReviewEvent, ReviewOutcome, Stage, StagePhase};
use saka_ports::{Checkout, PortError, PortResult, PrepareRequest, ReviewPost, clip_text};
use saka_spec::SpecModel;
use saka_store::read;
use serde_json::{Value, json};

use crate::review_packet::{self, ReviewBasisInput, ReviewPacketInput};
use crate::worker::{Effect, Execution, field, internal};
use crate::{Engine, EngineError};

const CONVENTION_FILES: [&str; 3] = ["AGENTS.md", "CONTRIBUTING.md", "CLAUDE.md"];
const CONVENTION_MAX_BYTES: usize = 16 * 1024;
const DIFF_MAX_BYTES: usize = 200 * 1024;

fn clipped(text: &str, max: usize) -> String {
    let cut = (0..=max.min(text.len()))
        .rev()
        .find(|i| text.is_char_boundary(*i))
        .unwrap_or(0);
    text[..cut].to_string()
}

impl Engine {
    pub(crate) async fn prepare_review(&self, task: &Task, run: StageRunId) -> Execution {
        let current = task
            .stage_run
            .as_ref()
            .filter(|r| r.id == run && r.stage == Stage::Review);
        let (Some(head_sha), Some(cr)) = (
            current.and_then(|r| r.head_sha.clone()),
            task.change_request.as_ref(),
        ) else {
            return Ok(Effect::Skipped);
        };
        if !task.is_at(Stage::Review, StagePhase::PreparingEnvironment)
            || task.ready_environment().is_some()
        {
            return Ok(Effect::Skipped);
        }
        let repo = self.repo_ref_of(task.item.repo_id)?;
        let head_ref = self.ports.scm.capabilities(&repo.integration_id).map(|c| {
            c.change_head_ref
                .replace("{number}", &cr.number.to_string())
        });
        // One environment per stage run: a repeated job finds the one it made.
        let environment_id = EnvironmentId(run.0);
        let prepared = self
            .ports
            .env
            .prepare(&PrepareRequest {
                environment_id,
                repo,
                checkout: Checkout::Change {
                    head_sha: head_sha.clone(),
                    head_ref,
                },
                base_branch: cr.base_branch.clone(),
            })
            .await?;
        Ok(Effect::Fact(TaskInput::ReviewEnvironmentPrepared {
            environment_id,
            root_path: prepared.root_path,
            worktree_path: prepared.worktree_path,
            head_sha,
        }))
    }

    pub(crate) async fn post_review_job(&self, task: &Task, payload: &Value) -> Execution {
        let review_id: saka_core::ids::ReviewId = field(payload, "review_id")?;
        let Some(review) = task.unposted_review().filter(|r| r.id == review_id) else {
            return Ok(Effect::Skipped);
        };
        let (Some(cr), true) = (
            task.change_request.as_ref(),
            task.is_at(Stage::Review, StagePhase::PostingReview),
        ) else {
            return Ok(Effect::Skipped);
        };
        let event = match review.outcome {
            ReviewOutcome::Approved => ReviewEvent::Approve,
            ReviewOutcome::ChangesRequested => ReviewEvent::RequestChanges,
            ReviewOutcome::SpecUnclear => return Ok(Effect::Skipped),
        };
        let verdict = review::Verdict {
            outcome: review.outcome,
            summary: review.summary.clone(),
            findings: review.findings.clone(),
            questions: review.questions.clone(),
        };
        let posted = self
            .ports
            .scm
            .post_review(&ReviewPost {
                repo: self.repo_ref_of(task.item.repo_id)?,
                number: cr.number,
                head_sha: review.head_sha.clone(),
                event,
                body: review::render_body(&review.head_sha, &verdict, review.basis),
            })
            .await?;
        Ok(Effect::Fact(TaskInput::ReviewPosted {
            review_id,
            event,
            url: posted.url.or_else(|| Some(cr.url.clone())),
        }))
    }

    pub(crate) async fn review_packet_text(&self, task: &Task) -> PortResult<String> {
        let (Some(cr), Some(run)) = (task.change_request.as_ref(), task.stage_run.as_ref()) else {
            return Err(PortError::permanent("review_packet_unavailable", json!({})));
        };
        let head_sha = run.head_sha.clone().unwrap_or_else(|| cr.head_sha.clone());
        let repo = self.repo_ref_of(task.item.repo_id)?;
        let summary = self.ports.scm.change_summary(&repo, cr.number).await?;
        let diff = self
            .ports
            .env
            .change_diff(&repo, &cr.base_branch, &head_sha, DIFF_MAX_BYTES)
            .await?;
        let mut conventions = Vec::new();
        if let Some(tip) = self.ports.env.branch_tip(&repo, &cr.base_branch).await? {
            for path in CONVENTION_FILES {
                if let Some(text) = self.ports.env.read_repo_file(&repo, &tip, path).await? {
                    conventions.push((path.to_string(), clipped(&text, CONVENTION_MAX_BYTES)));
                }
            }
        }
        let basis = self.review_basis_input(task).map_err(internal)?;
        let packet = review_packet::render(&ReviewPacketInput {
            basis,
            conventions,
            title: clip_text(&summary.title),
            description: clip_text(&summary.body),
            author: summary.author.clone(),
            from_fork: summary.from_fork,
            base_branch: cr.base_branch.clone(),
            head_sha: head_sha.clone(),
            diff: diff.text,
            diff_truncated: diff.truncated,
            files: diff.files,
        });
        tracing::info!(
            work_item_id = %task.item.id,
            bytes = packet.text.len(),
            sha256 = %packet.sha256,
            diff_bytes = packet.diff_bytes,
            truncated = packet.truncated,
            "review.packet"
        );
        Ok(packet.text)
    }

    fn review_basis_input(&self, task: &Task) -> crate::Result<ReviewBasisInput> {
        let Some(link) = task.external.as_ref().and_then(|e| e.spec.as_ref()) else {
            return Ok(ReviewBasisInput::None);
        };
        let (spec, revision) = self.store.read(|c| {
            let spec = read::load_spec(c, link.spec_id)?;
            let revision = match &spec {
                Some(s) => {
                    read::load_revision(c, s.approved_revision_id.unwrap_or(s.latest_revision_id))?
                }
                None => None,
            };
            Ok((spec, revision))
        })?;
        let (Some(spec), Some(revision)) = (spec, revision) else {
            return Ok(ReviewBasisInput::None);
        };
        let model: SpecModel = serde_json::from_value(revision.model.clone())
            .map_err(|e| EngineError::Internal(format!("revision model: {e}")))?;
        let step = link
            .step_id
            .as_deref()
            .and_then(|id| model.steps.iter().find(|s| s.id == id));
        Ok(match step {
            Some(step) => ReviewBasisInput::Step {
                spec_path: spec.path,
                work: step.work.clone(),
                criteria: step
                    .ac
                    .iter()
                    .filter_map(|id| model.acceptance_criteria.iter().find(|a| &a.id == id))
                    .map(|a| (a.id.clone(), a.text.clone()))
                    .collect(),
                commands: step.commands.clone(),
            },
            None => ReviewBasisInput::Spec {
                path: spec.path,
                content: String::from_utf8_lossy(&revision.content).into_owned(),
            },
        })
    }
}
