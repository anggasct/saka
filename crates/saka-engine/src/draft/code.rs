use std::collections::BTreeMap;
use std::time::Instant;

use saka_core::ids::EnvironmentId;
use saka_core::model::{Draft, Project};
use saka_ports::{CodeCheckout, CodeRequest, ExtraDir, PortError, code_path};

use crate::Engine;
use crate::packet::{CodeEntry, CodeSkip};

const BASE_BRANCH_MISSING: &str = "base_branch_missing";

#[derive(Debug, Default)]
pub(super) struct CodeSet {
    pub entries: Vec<CodeEntry>,
    pub skipped: Vec<CodeSkip>,
}

impl CodeSet {
    pub fn commits(&self) -> BTreeMap<String, String> {
        self.entries
            .iter()
            .map(|entry| (entry.binding.clone(), entry.sha.clone()))
            .collect()
    }
}

fn code_bindings(project: &Project) -> impl Iterator<Item = &saka_core::model::ProjectRepo> {
    project
        .repos
        .iter()
        .filter(|b| b.repo_id != project.workspace_repo_id)
}

impl Engine {
    pub(super) async fn prepare_code(
        &self,
        d: &Draft,
        project: &Project,
        environment_id: EnvironmentId,
    ) -> Result<CodeSet, PortError> {
        let mut set = CodeSet::default();
        for binding in code_bindings(project) {
            let repo = self.repo_ref_of(binding.repo_id)?;
            let started = Instant::now();
            let outcome = self
                .ports
                .env
                .prepare_code(&CodeRequest {
                    environment_id,
                    repo: repo.clone(),
                    name: binding.name.clone(),
                    base_branch: binding.base_branch.clone(),
                })
                .await;
            let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
            let (label, files, bytes) = match &outcome {
                Ok(CodeCheckout::Ready(p)) => ("ready", p.files, p.bytes),
                Ok(CodeCheckout::TooLarge { files, bytes, .. }) => ("too_large", *files, *bytes),
                Err(e) if e.code == BASE_BRANCH_MISSING => (BASE_BRANCH_MISSING, 0, 0),
                Err(_) => ("failed", 0, 0),
            };
            self.counters.architect_code_checkout(label);
            tracing::info!(
                spec_draft_id = %d.draft.id,
                binding = %binding.name,
                outcome = label,
                files,
                bytes,
                duration_ms,
                "architect.code_checkout"
            );
            match outcome {
                Ok(CodeCheckout::Ready(prepared)) => {
                    set.entries.push(CodeEntry {
                        binding: binding.name.clone(),
                        path: prepared.path,
                        full_name: repo.full_name,
                        sha: prepared.base_sha,
                        branch: binding.base_branch.clone(),
                    });
                }
                Ok(CodeCheckout::TooLarge { .. }) => set.skipped.push(CodeSkip {
                    binding: binding.name.clone(),
                    full_name: repo.full_name,
                    reason: label.to_string(),
                }),
                Err(e) if e.code == BASE_BRANCH_MISSING => set.skipped.push(CodeSkip {
                    binding: binding.name.clone(),
                    full_name: repo.full_name,
                    reason: label.to_string(),
                }),
                Err(e) => return Err(e),
            }
        }
        Ok(set)
    }

    pub(super) async fn teardown_code(
        &self,
        project: &Project,
        environment_id: EnvironmentId,
    ) -> Result<(), PortError> {
        let mut first = None;
        for binding in code_bindings(project) {
            let Ok(repo) = self.repo_ref_of(binding.repo_id) else {
                continue;
            };
            if let Err(e) = self
                .ports
                .env
                .teardown_code(environment_id, &repo, &binding.name)
                .await
            {
                first.get_or_insert(e);
            }
        }
        first.map_or(Ok(()), Err)
    }

    pub(crate) fn code_extra_dirs(&self, d: &Draft) -> Vec<ExtraDir> {
        let (Some(attempt), Some(env)) = (&d.attempt, &d.environment) else {
            return Vec::new();
        };
        attempt
            .code_commits
            .keys()
            .map(|name| ExtraDir {
                path: code_path(&env.root_path, name),
                writable: false,
            })
            .collect()
    }
}
