use std::collections::BTreeMap;

use saka_core::hook::{Hook, HookRun};
use saka_core::ids::{EnvironmentId, ProjectId, RepoId};
use saka_core::secret::{SecretScope, SecretVar};
use saka_core::vocab::{HookRunState, SecretPurpose};
use saka_ports::{
    Checkout, EnvRef, HostCommand, PortError, PrepareRequest, PreparedEnvironment, RecipeCommand,
    RecipePhase, RepoRef, Sealed,
};
use saka_store::read;
use serde_json::json;

use crate::Engine;
use crate::hooks::{HookReport, empty_result, failed};
use crate::recipe::{self, Recipe};
use crate::worker::Refusal;

pub(crate) struct FailureCodes {
    pub secret_missing: &'static str,
    pub environment_failed: &'static str,
    pub commit_unavailable: &'static str,
    pub timed_out: &'static str,
    pub command_failed: &'static str,
}

pub(crate) const HOOK_CODES: FailureCodes = FailureCodes {
    secret_missing: "hook_secret_missing",
    environment_failed: "hook_environment_failed",
    commit_unavailable: "hook_commit_unavailable",
    timed_out: "hook_timed_out",
    command_failed: "hook_command_failed",
};

pub(crate) struct HostRun<'a> {
    pub name: String,
    pub command: &'a str,
    pub cwd: Option<&'a str>,
    pub env: BTreeMap<String, String>,
    pub scratch_key: String,
    pub timeout_ms: u64,
}

pub(crate) struct EnvSpec<'a> {
    pub id: uuid::Uuid,
    pub project_id: ProjectId,
    pub repo_id: RepoId,
    pub base_branch: String,
    pub sha: Option<String>,
    pub secrets: &'a [String],
    pub vars: Vec<(String, String)>,
}

pub(crate) struct OpenEnvironment {
    env: EnvRef,
}

pub(crate) enum EnvFailure {
    Retry(PortError),
    Failed(HookReport),
}

pub(crate) fn command_report(
    exit_code: Option<i32>,
    timed_out: bool,
    duration_ms: u64,
    log: &str,
    tail: &str,
    timeout_ms: u64,
    codes: &FailureCodes,
) -> HookReport {
    let mut result = empty_result();
    result["exit_code"] = json!(exit_code);
    result["timed_out"] = json!(timed_out);
    result["log"] = json!(log);
    result["output_tail"] = json!(tail);
    let duration_ms = i64::try_from(duration_ms).unwrap_or(i64::MAX);
    if !timed_out && exit_code == Some(0) {
        return HookReport {
            outcome: HookRunState::Succeeded,
            duration_ms,
            result,
            error: None,
        };
    }
    if timed_out {
        return failed(
            codes.timed_out,
            json!({ "timeout_secs": timeout_ms / 1000 }),
            result,
            duration_ms,
        );
    }
    failed(
        codes.command_failed,
        json!({ "exit_code": exit_code }),
        result,
        duration_ms,
    )
}

pub(crate) fn environment_failed(error: &PortError, codes: &FailureCodes) -> HookReport {
    failed(
        codes.environment_failed,
        json!({ "error": error.to_json() }),
        empty_result(),
        0,
    )
}

impl Engine {
    pub(crate) async fn hook_on_host(
        &self,
        run: &HookRun,
        hook: &Hook,
        command: &str,
        cwd: Option<&str>,
    ) -> Result<HookReport, PortError> {
        let env: BTreeMap<String, String> = run
            .context
            .env_vars(run.id, &run.hook)
            .into_iter()
            .collect();
        self.host_command(
            HostRun {
                name: run.id.to_string(),
                command,
                cwd,
                env,
                scratch_key: format!("{}/{}", run.project_id, hook.name),
                timeout_ms: hook.timeout_ms,
            },
            &HOOK_CODES,
        )
        .await
    }

    pub(crate) async fn host_command(
        &self,
        run: HostRun<'_>,
        codes: &FailureCodes,
    ) -> Result<HookReport, PortError> {
        let request = HostCommand {
            name: run.name,
            command: run.command.to_string(),
            cwd: run.cwd.map(str::to_string),
            env: run.env,
            scratch_key: run.scratch_key,
            timeout_ms: run.timeout_ms,
        };
        match self.ports.host.run(&request).await {
            Ok(o) => Ok(command_report(
                o.exit_code,
                o.timed_out,
                o.duration_ms,
                &o.log_path,
                &o.output_tail,
                run.timeout_ms,
                codes,
            )),
            Err(e) if e.retryable => Err(e),
            Err(e) => Ok(failed(&e.code, e.params, empty_result(), 0)),
        }
    }

    pub(crate) async fn hook_in_environment(
        &self,
        run: &HookRun,
        hook: &Hook,
        command: &str,
        secret_names: &[String],
    ) -> Result<HookReport, PortError> {
        let Some((repo_id, base_branch)) = self.hook_repo(run, hook)? else {
            return Ok(failed(
                "hook_commit_unavailable",
                json!({}),
                empty_result(),
                0,
            ));
        };
        let spec = EnvSpec {
            id: run.id.0,
            project_id: run.project_id,
            repo_id,
            base_branch,
            sha: run.sha.clone(),
            secrets: secret_names,
            vars: run.context.env_vars(run.id, &run.hook),
        };
        let opened = match self.open_environment(&spec, &HOOK_CODES).await {
            Ok(opened) => opened,
            Err(EnvFailure::Retry(e)) => return Err(e),
            Err(EnvFailure::Failed(report)) => return Ok(report),
        };
        let result = self
            .run_in_environment(&opened, 0, command, hook.timeout_ms, &HOOK_CODES)
            .await;
        self.close_environment(&opened, &run.id.to_string()).await;
        result
    }

    pub(crate) async fn open_environment(
        &self,
        spec: &EnvSpec<'_>,
        codes: &FailureCodes,
    ) -> Result<OpenEnvironment, EnvFailure> {
        let secrets = match self.project_secret_vars(spec.project_id, spec.secrets) {
            Ok(Ok(secrets)) => secrets,
            Ok(Err(name)) => {
                return Err(EnvFailure::Failed(failed(
                    codes.secret_missing,
                    json!({ "name": name }),
                    empty_result(),
                    0,
                )));
            }
            Err(e) => return Err(EnvFailure::Retry(e)),
        };
        let repo = self.repo_ref_of(spec.repo_id).map_err(EnvFailure::Retry)?;
        let sha = match &spec.sha {
            Some(sha) => sha.clone(),
            None => match self.ports.env.branch_tip(&repo, &spec.base_branch).await {
                Ok(Some(sha)) => sha,
                Ok(None) => {
                    return Err(EnvFailure::Failed(failed(
                        codes.commit_unavailable,
                        json!({ "branch": spec.base_branch }),
                        empty_result(),
                        0,
                    )));
                }
                Err(e) if e.retryable => return Err(EnvFailure::Retry(e)),
                Err(e) => return Err(EnvFailure::Failed(environment_failed(&e, codes))),
            },
        };
        let environment_id = EnvironmentId(spec.id);
        let prepared = match self
            .ports
            .env
            .prepare(&PrepareRequest {
                environment_id,
                repo: repo.clone(),
                checkout: Checkout::Detached,
                base_branch: spec.base_branch.clone(),
            })
            .await
        {
            Ok(p) => p,
            Err(e) if e.retryable => return Err(EnvFailure::Retry(e)),
            Err(e) => return Err(EnvFailure::Failed(environment_failed(&e, codes))),
        };
        let mut env = EnvRef {
            environment_id,
            repo: repo.clone(),
            branch: None,
            root_path: prepared.root_path.clone(),
            worktree_path: prepared.worktree_path.clone(),
            vars: BTreeMap::new(),
            path_prefix: Vec::new(),
            secrets,
            extra_dirs: Vec::new(),
        };
        match self
            .prepare_checkout(spec, &repo, &sha, &prepared, &mut env, codes)
            .await
        {
            Ok(()) => Ok(OpenEnvironment { env }),
            Err(failure) => {
                self.close_environment(&OpenEnvironment { env }, &spec.id.to_string())
                    .await;
                Err(failure)
            }
        }
    }

    async fn prepare_checkout(
        &self,
        spec: &EnvSpec<'_>,
        repo: &RepoRef,
        sha: &str,
        prepared: &PreparedEnvironment,
        env: &mut EnvRef,
        codes: &FailureCodes,
    ) -> Result<(), EnvFailure> {
        match self.ports.env.check_out(env, sha).await {
            Ok(()) => {}
            Err(e) if e.retryable => return Err(EnvFailure::Retry(e)),
            Err(e) if e.code == "commit_missing" => {
                return Err(EnvFailure::Failed(failed(
                    codes.commit_unavailable,
                    json!({ "commit": sha }),
                    empty_result(),
                    0,
                )));
            }
            Err(e) => return Err(EnvFailure::Failed(environment_failed(&e, codes))),
        }
        let text = match self.ports.env.read_recipe(repo, &prepared.base_sha).await {
            Ok(text) => text,
            Err(e) if e.retryable => return Err(EnvFailure::Retry(e)),
            Err(e) => return Err(EnvFailure::Failed(environment_failed(&e, codes))),
        };
        let recipe = match text.as_deref() {
            None => Recipe::default(),
            Some(t) => match recipe::parse(t, self.cfg.env.ports_per_env) {
                Ok(r) => r,
                Err(issues) => {
                    let first = issues.first().map(|i| i.to_json());
                    return Err(EnvFailure::Failed(failed(
                        codes.environment_failed,
                        json!({ "error": { "code": "env_recipe_invalid", "params": first } }),
                        empty_result(),
                        0,
                    )));
                }
            },
        };
        let toolchains = match self
            .install_toolchains(repo, &prepared.base_sha, &recipe)
            .await
        {
            Ok(t) => t,
            Err(Refusal::Fail(e)) if e.retryable => return Err(EnvFailure::Retry(e)),
            Err(refusal) => {
                return Err(EnvFailure::Failed(environment_failed(
                    &refusal.into_error(),
                    codes,
                )));
            }
        };
        let mut vars = recipe.env.clone();
        for t in &toolchains {
            vars.extend(t.env.clone());
        }
        vars.extend(spec.vars.clone());
        env.vars = vars;
        env.path_prefix = toolchains.iter().map(|t| t.bin_dir.clone()).collect();
        Ok(())
    }

    pub(crate) async fn run_in_environment(
        &self,
        opened: &OpenEnvironment,
        index: usize,
        command: &str,
        timeout_ms: u64,
        codes: &FailureCodes,
    ) -> Result<HookReport, PortError> {
        let outcome = self
            .ports
            .env
            .run_recipe_command(
                &opened.env,
                &RecipeCommand {
                    phase: RecipePhase::Hook,
                    index,
                    command: command.to_string(),
                    run: None,
                    timeout_ms: Some(timeout_ms),
                },
            )
            .await?;
        Ok(command_report(
            outcome.exit_code,
            outcome.timed_out,
            outcome.duration_ms,
            &outcome.log_path,
            &outcome.output_tail,
            timeout_ms,
            codes,
        ))
    }

    pub(crate) async fn close_environment(&self, opened: &OpenEnvironment, owner: &str) {
        if let Err(e) = self.ports.env.teardown(&opened.env).await {
            tracing::warn!(owner, code = %e.code, "environment.teardown_failed");
        }
    }

    fn hook_repo(&self, run: &HookRun, hook: &Hook) -> Result<Option<(RepoId, String)>, PortError> {
        let project = self
            .store
            .read(|c| read::load_project(c, run.project_id))
            .map_err(|e| crate::worker::internal(e.into()))?
            .ok_or_else(|| PortError::permanent("project_not_found", json!({})))?;
        let binding = match run.repo_id {
            Some(id) => project.repos.iter().find(|b| b.repo_id == id),
            None => hook
                .filter
                .repos
                .first()
                .and_then(|name| project.repos.iter().find(|b| &b.name == name)),
        };
        Ok(binding.map(|b| (b.repo_id, b.base_branch.clone())))
    }

    pub(crate) fn project_secret_vars(
        &self,
        project_id: ProjectId,
        names: &[String],
    ) -> Result<Result<Vec<SecretVar>, String>, PortError> {
        if names.is_empty() {
            return Ok(Ok(Vec::new()));
        }
        let rows = self
            .store
            .read(|c| read::project_secrets(c, project_id))
            .map_err(|e| {
                PortError::retryable("store_unavailable", json!({ "detail": e.to_string() }))
            })?;
        let mut out = Vec::new();
        for name in names {
            let Some(row) = rows.iter().find(|r| &r.name == name) else {
                return Ok(Err(name.clone()));
            };
            let sealed = Sealed {
                nonce: row.nonce.clone(),
                ciphertext: row.ciphertext.clone(),
            };
            let scope = SecretScope::Project { project_id, name };
            match self.ports.secrets.open(&scope, &sealed) {
                Ok(value) => out.push(SecretVar {
                    name: name.clone(),
                    value,
                    purposes: vec![SecretPurpose::Setup],
                }),
                Err(e) if e.retryable => return Err(e),
                Err(_) => return Ok(Err(name.clone())),
            }
        }
        Ok(Ok(out))
    }
}
