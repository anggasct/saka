use saka_core::ids::{EnvironmentId, StageRunId};
use saka_core::model::Task;
use saka_core::review;
use saka_core::task::TaskInput;
use saka_core::vocab::{ReviewEvent, ReviewOutcome, Stage, StagePhase, WorkItemOrigin};
use saka_ports::{Checkout, PortError, PortResult, PrepareRequest, ReviewPost, clip_text};
use saka_spec::SpecModel;
use saka_store::read;
use serde_json::{Value, json};

use crate::review_packet::{
    self, ReviewBasisInput, ReviewKindInput, ReviewPacketInput, ReviewRegression,
};
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
        // The provider refuses an approval of a change opened by the same account.
        let own = task.item.origin == WorkItemOrigin::SpecStep;
        let event = match review.outcome {
            ReviewOutcome::SpecUnclear => return Ok(Effect::Skipped),
            _ if own => ReviewEvent::Comment,
            ReviewOutcome::Approved => ReviewEvent::Approve,
            ReviewOutcome::ChangesRequested => ReviewEvent::RequestChanges,
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
        let kind = self.review_kind_input(task, &head_sha).map_err(internal)?;
        let packet = review_packet::render(&ReviewPacketInput {
            kind,
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

    fn review_kind_input(&self, task: &Task, head_sha: &str) -> crate::Result<ReviewKindInput> {
        use saka_core::vocab::{GateSource, WorkKind};
        match task.item.kind.filter(|_| task.external.is_none()) {
            Some(WorkKind::SecurityPatch) => Ok(ReviewKindInput::SecurityPatch),
            Some(WorkKind::Bugfix) => {
                let commands = self.pinned(task)?.regression().to_vec();
                let gate = task.gate_for(GateSource::Saka, head_sha);
                let evidence = task.reproduced_regression().map(|check| ReviewRegression {
                    head_passed: commands
                        .iter()
                        .map(|command| {
                            let passed = gate.is_some_and(|g| {
                                g.checks.iter().any(|c| {
                                    c.name == crate::worker::check_name(command)
                                        && !c.is_failure()
                                        && c.conclusion.is_some()
                                })
                            });
                            (command.clone(), passed)
                        })
                        .collect(),
                    commands,
                    test_commit: check.commit_sha.clone(),
                    checks: check.checks.clone(),
                });
                Ok(ReviewKindInput::Bugfix(evidence))
            }
            _ => Ok(ReviewKindInput::Plain),
        }
    }

    fn review_basis_input(&self, task: &Task) -> crate::Result<ReviewBasisInput> {
        let link = task
            .external
            .as_ref()
            .and_then(|e| e.spec.as_ref())
            .map(|s| (s.spec_id, s.step_id.clone()))
            .or_else(|| task.item.spec_id.map(|id| (id, task.item.step_id.clone())));
        let Some((spec_id, step_id)) = link else {
            return Ok(ReviewBasisInput::None);
        };
        let (spec, current, pinned) = self.store.read(|c| {
            let spec = read::load_spec(c, spec_id)?;
            let current = match &spec {
                Some(s) => {
                    read::load_revision(c, s.approved_revision_id.unwrap_or(s.latest_revision_id))?
                }
                None => None,
            };
            let pinned = match task.item.spec_revision_id {
                Some(id) => read::load_revision(c, id)?,
                None => None,
            };
            Ok((spec, current, pinned))
        })?;
        let Some(spec) = spec else {
            return Ok(ReviewBasisInput::None);
        };
        let models = [current, pinned].into_iter().flatten().map(|revision| {
            serde_json::from_value::<SpecModel>(revision.model.clone())
                .map(|model| (revision, model))
                .map_err(|e| EngineError::Internal(format!("revision model: {e}")))
        });
        let mut first = None;
        let mut found = None;
        for entry in models {
            let (revision, model) = entry?;
            let has_step = step_id
                .as_deref()
                .is_some_and(|id| model.steps.iter().any(|s| s.id == id));
            if has_step {
                found = Some((revision, model));
                break;
            }
            first.get_or_insert((revision, model));
        }
        let Some((revision, model)) = found.or(first) else {
            return Ok(ReviewBasisInput::None);
        };
        let step = step_id
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
