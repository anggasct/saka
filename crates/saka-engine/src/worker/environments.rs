use std::collections::BTreeMap;

use saka_core::ids::{EnvironmentId, StageRunId};
use saka_core::model::{DocsCheckout, Environment, EnvironmentSetup, Task, ToolchainUse};
use saka_core::task::TaskInput;
use saka_core::vocab::{BlockerCode, DocsMode, EnvironmentState, WorkItemOrigin};
use saka_ports::{
    Checkout, EnvRef, PortError, PrepareRequest, RecipeCommand, RecipePhase, RepoRef,
};
use saka_store::read;
use serde_json::{Value, json};

use super::{Effect, Execution, Refusal, extra_dirs, internal};
use crate::Engine;
use crate::recipe::{self, Recipe};
use crate::toolchain;

impl Engine {
    pub(super) fn env_ref(
        &self,
        task: &Task,
        env: &Environment,
    ) -> std::result::Result<EnvRef, PortError> {
        let recipe = env
            .recipe
            .as_ref()
            .map(Recipe::from_json)
            .unwrap_or_default();
        Ok(EnvRef {
            environment_id: env.id,
            repo: self.repo_ref(task)?,
            branch: env.branch.clone(),
            root_path: env.root_path.clone(),
            worktree_path: env.worktree_path.clone(),
            vars: self.env_vars(
                task,
                env.id,
                env.port_start,
                &recipe,
                &env.toolchains,
                env.docs.as_ref(),
            ),
            path_prefix: env.toolchains.iter().map(|t| t.bin_dir.clone()).collect(),
            secrets: Vec::new(),
            extra_dirs: extra_dirs(env.docs.as_ref()),
        })
    }

    pub(super) fn env_ref_with_secrets(
        &self,
        task: &Task,
        env: &Environment,
    ) -> std::result::Result<EnvRef, Refusal> {
        let mut env_ref = self.env_ref(task, env).map_err(Refusal::Fail)?;
        let recipe = env
            .recipe
            .as_ref()
            .map(Recipe::from_json)
            .unwrap_or_default();
        env_ref.secrets = self.recipe_secrets(task.item.project_id, &recipe)?;
        Ok(env_ref)
    }

    pub(super) fn env_vars(
        &self,
        task: &Task,
        environment_id: EnvironmentId,
        port_start: Option<u16>,
        recipe: &Recipe,
        toolchains: &[ToolchainUse],
        docs: Option<&DocsCheckout>,
    ) -> BTreeMap<String, String> {
        let mut vars = recipe.env.clone();
        for t in toolchains {
            vars.extend(t.env.clone());
        }
        if let Some(docs) = docs {
            vars.insert("SAKA_DOCS_DIR".into(), docs.path.clone());
        }
        vars.insert("SAKA_TASK_KEY".into(), task.item.task_key.clone());
        vars.insert("SAKA_ENVIRONMENT_ID".into(), environment_id.to_string());
        let wanted = recipe.ports_or(self.cfg.env.ports_per_env);
        if let Some(start) = port_start.filter(|_| wanted > 0) {
            vars.insert("SAKA_PORT_BASE".into(), start.to_string());
            vars.insert("SAKA_PORT_COUNT".into(), wanted.to_string());
            vars.insert("PORT".into(), start.to_string());
        }
        vars
    }

    pub(super) fn reserve_ports(
        &self,
        environment_id: EnvironmentId,
    ) -> std::result::Result<u16, PortError> {
        let live = self
            .store
            .read(read::live_port_blocks)
            .map_err(|e| internal(e.into()))?;
        let mut in_flight = self
            .ports_in_flight
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(start) = in_flight.get(&environment_id) {
            return Ok(*start);
        }
        let taken = |start: u16| {
            live.iter().any(|(s, _)| *s == start) || in_flight.values().any(|s| *s == start)
        };
        let start = (0..self.cfg.env.port_blocks())
            .filter_map(|i| self.cfg.env.block_start(i))
            .find(|s| !taken(*s))
            .ok_or_else(|| {
                PortError::retryable(
                    "port_block_unavailable",
                    json!({ "blocks": self.cfg.env.port_blocks() }),
                )
            })?;
        in_flight.insert(environment_id, start);
        Ok(start)
    }

    pub(super) fn release_ports(&self, environment_id: EnvironmentId) {
        self.ports_in_flight
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&environment_id);
    }

    pub(super) async fn run_teardown_commands(&self, env: &EnvRef, recipe: &Recipe) -> Vec<Value> {
        let mut failures = Vec::new();
        for (index, command) in recipe.teardown.iter().enumerate() {
            let run = RecipeCommand {
                phase: RecipePhase::Teardown,
                index,
                command: command.clone(),
                run: None,
                timeout_ms: None,
            };
            match self.ports.env.run_recipe_command(env, &run).await {
                Ok(o) if o.succeeded() => {}
                Ok(o) if o.timed_out => {
                    failures.push(json!({ "command_index": index, "timed_out": true }));
                }
                Ok(o) => failures.push(json!({ "command_index": index, "exit_code": o.exit_code })),
                Err(e) => failures.push(json!({ "command_index": index, "error": e.code })),
            }
        }
        failures
    }

    pub(super) async fn abandon_environment(
        &self,
        env: &EnvRef,
        docs: Option<&RepoRef>,
        recipe: &Recipe,
        code: BlockerCode,
        params: Value,
    ) -> Execution {
        self.run_teardown_commands(env, recipe).await;
        if let Some(repo) = docs {
            self.ports
                .env
                .teardown_docs(env.environment_id, repo)
                .await?;
        }
        self.ports.env.teardown(env).await?;
        Ok(Effect::Fact(TaskInput::EnvironmentFailed { code, params }))
    }

    pub(crate) async fn install_toolchains(
        &self,
        repo: &RepoRef,
        base_sha: &str,
        recipe: &Recipe,
    ) -> std::result::Result<Vec<ToolchainUse>, Refusal> {
        let mut files = toolchain::Files::default();
        for path in toolchain::FILES {
            match self.ports.env.read_repo_file(repo, base_sha, path).await {
                Ok(text) => files.set(path, text),
                Err(e) if !e.retryable => {
                    return Err(Refusal::Block(
                        BlockerCode::EnvSetupFailed,
                        json!({ "error": e.to_json() }),
                    ));
                }
                Err(e) => return Err(Refusal::Fail(e)),
            }
        }
        let wanted = toolchain::detect(&recipe.toolchain, &files)
            .map_err(|u| Refusal::Block(BlockerCode::ToolchainUnpinned, u.to_json()))?;
        let mut installed = Vec::new();
        for (tool, version) in wanted {
            match self.ports.toolchains.install(tool, &version).await {
                Ok(t) => installed.push(t),
                Err(e) if !e.retryable && e.code == "toolchain_unavailable" => {
                    return Err(Refusal::Block(BlockerCode::ToolchainUnavailable, e.params));
                }
                Err(e) => return Err(Refusal::Fail(e)),
            }
        }
        Ok(installed)
    }

    pub(super) async fn env_prepare(&self, task: &Task, run: StageRunId) -> Execution {
        let current = task.stage_run.as_ref().is_some_and(|r| r.id == run);
        if !current || task.ready_environment().is_some() {
            return Ok(Effect::Skipped);
        }
        let environment_id = EnvironmentId(run.0);
        let repo = self.repo_ref(task)?;
        let prepared = self
            .ports
            .env
            .prepare(&PrepareRequest {
                environment_id,
                repo: repo.clone(),
                checkout: Checkout::Branch(task.item.branch.clone()),
                base_branch: self.base_branch(task)?,
            })
            .await?;
        let path_scope = self.effective_scope(task)?;
        let mut env = EnvRef {
            environment_id,
            repo: repo.clone(),
            branch: Some(task.item.branch.clone()),
            root_path: prepared.root_path.clone(),
            worktree_path: prepared.worktree_path.clone(),
            vars: BTreeMap::new(),
            path_prefix: Vec::new(),
            secrets: Vec::new(),
            extra_dirs: Vec::new(),
        };
        if task.item.origin == WorkItemOrigin::ExternalChange
            && let Some(cr) = &task.change_request
        {
            self.ports.env.check_out(&env, &cr.head_sha).await?;
        }
        let request = self.docs_request(task, environment_id)?;
        let docs_repo = request.as_ref().map(|r| &r.repo);
        let docs = match &request {
            None => None,
            Some(request) => match self.ports.env.prepare_docs(request).await {
                Ok(prepared) => Some(DocsCheckout {
                    repo_id: request.repo.repo_id,
                    full_name: request.repo.full_name.clone(),
                    commit: self
                        .pinned(task)
                        .map_err(internal)?
                        .source_commit()
                        .to_string(),
                    project_path: request.project_path.clone(),
                    path: prepared.path,
                    mode: DocsMode::Writable,
                    branch: request.branch.clone(),
                    base_sha: prepared.base_sha,
                    pushed_sha: task.docs_change.as_ref().map(|c| c.head_sha.clone()),
                }),
                Err(e) if !e.retryable => {
                    let params = json!({ "error": e.to_json() });
                    return self
                        .abandon_environment(
                            &env,
                            docs_repo,
                            &Recipe::default(),
                            BlockerCode::EnvSetupFailed,
                            params,
                        )
                        .await;
                }
                Err(e) => return Err(e),
            },
        };
        env.extra_dirs = extra_dirs(docs.as_ref());
        let text = match self.ports.env.read_recipe(&repo, &prepared.base_sha).await {
            Ok(text) => text,
            Err(e) if !e.retryable => {
                let params = json!({ "code": e.code, "field": null, "line": null });
                return self
                    .abandon_environment(
                        &env,
                        docs_repo,
                        &Recipe::default(),
                        BlockerCode::EnvRecipeInvalid,
                        params,
                    )
                    .await;
            }
            Err(e) => return Err(e),
        };
        let parsed = match text.as_deref() {
            None => Ok(Recipe::default()),
            Some(t) => recipe::parse(t, self.cfg.env.ports_per_env),
        };
        let recipe = match parsed {
            Ok(r) => r,
            Err(issues) => {
                let first = issues.first();
                let params = json!({
                    "code": first.map(|i| i.code),
                    "field": first.and_then(|i| i.field.clone()),
                    "line": first.and_then(|i| i.line),
                    "issues": issues.iter().map(|i| i.to_json()).collect::<Vec<_>>(),
                });
                return self
                    .abandon_environment(
                        &env,
                        docs_repo,
                        &Recipe::default(),
                        BlockerCode::EnvRecipeInvalid,
                        params,
                    )
                    .await;
            }
        };
        env.secrets = match self.recipe_secrets(task.item.project_id, &recipe) {
            Ok(s) => s,
            Err(Refusal::Block(code, params)) => {
                return self
                    .abandon_environment(&env, docs_repo, &Recipe::default(), code, params)
                    .await;
            }
            Err(Refusal::Fail(e)) => return Err(e),
        };
        let toolchains = match self
            .install_toolchains(&repo, &prepared.base_sha, &recipe)
            .await
        {
            Ok(t) => t,
            Err(Refusal::Block(code, params)) => {
                return self
                    .abandon_environment(&env, docs_repo, &Recipe::default(), code, params)
                    .await;
            }
            Err(Refusal::Fail(e)) => return Err(e),
        };
        let port_start = self.reserve_ports(environment_id)?;
        env.vars = self.env_vars(
            task,
            environment_id,
            Some(port_start),
            &recipe,
            &toolchains,
            docs.as_ref(),
        );
        env.path_prefix = toolchains.iter().map(|t| t.bin_dir.clone()).collect();
        let mut setup = Vec::new();
        for (index, command) in recipe.setup.iter().enumerate() {
            let run = RecipeCommand {
                phase: RecipePhase::Setup,
                index,
                command: command.clone(),
                run: None,
                timeout_ms: None,
            };
            let outcome = self.ports.env.run_recipe_command(&env, &run).await?;
            setup.push(json!({
                "command_index": index,
                "exit_code": outcome.exit_code,
                "timed_out": outcome.timed_out,
                "duration_ms": outcome.duration_ms,
            }));
            if !outcome.succeeded() {
                let mut failed = json!({ "command_index": index, "log": outcome.log_path });
                if outcome.timed_out {
                    failed["timed_out"] = json!(true);
                } else {
                    failed["exit_code"] = json!(outcome.exit_code);
                }
                let params =
                    json!({ "error": { "code": "setup_command_failed", "params": failed } });
                return self
                    .abandon_environment(
                        &env,
                        docs_repo,
                        &recipe,
                        BlockerCode::EnvSetupFailed,
                        params,
                    )
                    .await;
            }
        }
        Ok(Effect::Fact(TaskInput::EnvironmentPrepared {
            environment_id,
            root_path: prepared.root_path,
            worktree_path: prepared.worktree_path,
            base_sha: prepared.base_sha,
            setup: Box::new(EnvironmentSetup {
                port_start: Some(port_start),
                port_count: Some(self.cfg.env.ports_per_env),
                recipe: text.as_ref().map(|_| recipe.to_json()),
                recipe_sha256: text.as_deref().map(recipe::sha256),
                setup,
                toolchains,
                path_scope,
                docs,
            }),
        }))
    }

    pub(super) async fn teardown(&self, task: &Task, id: EnvironmentId) -> Execution {
        let Some(env) = task
            .environment
            .as_ref()
            .filter(|e| e.id == id && e.state != EnvironmentState::TornDown)
        else {
            return Ok(Effect::Skipped);
        };
        let env_ref = match self.env_ref_with_secrets(task, env) {
            Ok(r) => r,
            Err(Refusal::Fail(e)) if e.retryable => return Err(e),
            Err(_) => self.env_ref(task, env)?,
        };
        // A cleanup retry after teardown_failed must not run the repo's teardown commands again.
        let failures = if env.state == EnvironmentState::TeardownFailed {
            Vec::new()
        } else {
            let recipe = env
                .recipe
                .as_ref()
                .map(Recipe::from_json)
                .unwrap_or_default();
            self.run_teardown_commands(&env_ref, &recipe).await
        };
        if let Some(docs) = &env.docs {
            let repo = self.repo_ref_of(docs.repo_id)?;
            self.ports.env.teardown_docs(env.id, &repo).await?;
        }
        self.ports.env.teardown(&env_ref).await?;
        Ok(Effect::Fact(TaskInput::EnvironmentTornDown {
            teardown_failures: failures,
        }))
    }
}
