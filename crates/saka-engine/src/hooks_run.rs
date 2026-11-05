use std::collections::BTreeMap;

use saka_core::hook::{Hook, HookRun};
use saka_core::ids::{EnvironmentId, RepoId};
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

fn command_report(
    exit_code: Option<i32>,
    timed_out: bool,
    duration_ms: u64,
    log: &str,
    tail: &str,
    timeout_ms: u64,
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
            "hook_timed_out",
            json!({ "timeout_secs": timeout_ms / 1000 }),
            result,
            duration_ms,
        );
    }
    failed(
        "hook_command_failed",
        json!({ "exit_code": exit_code }),
        result,
        duration_ms,
    )
}

fn environment_failed(error: &PortError) -> HookReport {
    failed(
        "hook_environment_failed",
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
        let request = HostCommand {
            name: run.id.to_string(),
            command: command.to_string(),
            cwd: cwd.map(str::to_string),
            env,
            scratch_key: format!("{}/{}", run.project_id, hook.name),
            timeout_ms: hook.timeout_ms,
        };
        match self.ports.host.run(&request).await {
            Ok(o) => Ok(command_report(
                o.exit_code,
                o.timed_out,
                o.duration_ms,
                &o.log_path,
                &o.output_tail,
                hook.timeout_ms,
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
        let secrets = match self.hook_secrets(run, secret_names)? {
            Ok(secrets) => secrets,
            Err(name) => {
                return Ok(failed(
                    "hook_secret_missing",
                    json!({ "name": name }),
                    empty_result(),
                    0,
                ));
            }
        };
        let (repo_id, base_branch) = match self.hook_repo(run, hook)? {
            Some(found) => found,
            None => {
                return Ok(failed(
                    "hook_commit_unavailable",
                    json!({}),
                    empty_result(),
                    0,
                ));
            }
        };
        let repo = self.repo_ref_of(repo_id)?;
        let sha = match &run.sha {
            Some(sha) => sha.clone(),
            None => match self.ports.env.branch_tip(&repo, &base_branch).await {
                Ok(Some(sha)) => sha,
                Ok(None) => {
                    return Ok(failed(
                        "hook_commit_unavailable",
                        json!({ "branch": base_branch }),
                        empty_result(),
                        0,
                    ));
                }
                Err(e) if e.retryable => return Err(e),
                Err(e) => return Ok(environment_failed(&e)),
            },
        };
        let environment_id = EnvironmentId(run.id.0);
        let prepared = match self
            .ports
            .env
            .prepare(&PrepareRequest {
                environment_id,
                repo: repo.clone(),
                checkout: Checkout::Detached,
                base_branch,
            })
            .await
        {
            Ok(p) => p,
            Err(e) if e.retryable => return Err(e),
            Err(e) => return Ok(environment_failed(&e)),
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
        let result = self
            .hook_command(run, hook, command, &repo, &sha, &prepared, &mut env)
            .await;
        if let Err(e) = self.ports.env.teardown(&env).await {
            tracing::warn!(hook_run_id = %run.id, code = %e.code, "hook.environment.teardown_failed");
        }
        result
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "one step of the environment run, with its context passed through"
    )]
    async fn hook_command(
        &self,
        run: &HookRun,
        hook: &Hook,
        command: &str,
        repo: &RepoRef,
        sha: &str,
        prepared: &PreparedEnvironment,
        env: &mut EnvRef,
    ) -> Result<HookReport, PortError> {
        match self.ports.env.check_out(env, sha).await {
            Ok(()) => {}
            Err(e) if e.retryable => return Err(e),
            Err(e) if e.code == "commit_missing" => {
                return Ok(failed(
                    "hook_commit_unavailable",
                    json!({ "commit": sha }),
                    empty_result(),
                    0,
                ));
            }
            Err(e) => return Ok(environment_failed(&e)),
        }
        let text = match self.ports.env.read_recipe(repo, &prepared.base_sha).await {
            Ok(text) => text,
            Err(e) if e.retryable => return Err(e),
            Err(e) => return Ok(environment_failed(&e)),
        };
        let recipe = match text.as_deref() {
            None => Recipe::default(),
            Some(t) => match recipe::parse(t, self.cfg.env.ports_per_env) {
                Ok(r) => r,
                Err(issues) => {
                    let first = issues.first().map(|i| i.to_json());
                    return Ok(failed(
                        "hook_environment_failed",
                        json!({ "error": { "code": "env_recipe_invalid", "params": first } }),
                        empty_result(),
                        0,
                    ));
                }
            },
        };
        let toolchains = match self
            .install_toolchains(repo, &prepared.base_sha, &recipe)
            .await
        {
            Ok(t) => t,
            Err(Refusal::Fail(e)) if e.retryable => return Err(e),
            Err(refusal) => return Ok(environment_failed(&refusal.into_error())),
        };
        let mut vars = recipe.env.clone();
        for t in &toolchains {
            vars.extend(t.env.clone());
        }
        vars.extend(run.context.env_vars(run.id, &run.hook));
        env.vars = vars;
        env.path_prefix = toolchains.iter().map(|t| t.bin_dir.clone()).collect();
        let outcome = self
            .ports
            .env
            .run_recipe_command(
                env,
                &RecipeCommand {
                    phase: RecipePhase::Hook,
                    index: 0,
                    command: command.to_string(),
                    run: None,
                    timeout_ms: Some(hook.timeout_ms),
                },
            )
            .await?;
        Ok(command_report(
            outcome.exit_code,
            outcome.timed_out,
            outcome.duration_ms,
            &outcome.log_path,
            &outcome.output_tail,
            hook.timeout_ms,
        ))
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

    fn hook_secrets(
        &self,
        run: &HookRun,
        names: &[String],
    ) -> Result<Result<Vec<SecretVar>, String>, PortError> {
        if names.is_empty() {
            return Ok(Ok(Vec::new()));
        }
        let rows = self
            .store
            .read(|c| read::project_secrets(c, run.project_id))
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
            let scope = SecretScope::Project {
                project_id: run.project_id,
                name,
            };
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
