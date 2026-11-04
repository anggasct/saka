# Saka

A self-hosted delivery loop for coding agents: an approved spec becomes one task per step, each task runs an agent session in its own worktree, and Saka opens, gates, and merges the pull request before starting the steps that wait for it.

## Running

```sh
cargo build --release -p saka --features embed-web   # after `cargo xtask wasm` and `corepack pnpm build` in web/
saka serve                                            # API, workers, and the web app on 127.0.0.1:7420
saka project add https://github.com/<owner>/<repo> --path docs
saka spec template > docs/specs/<spec>.md            # a spec that passes lint (--locale id for Indonesian)
saka spec approve <project> docs/specs/<spec>.md       # again after an edit: repins steps that have not started
saka spec withdraw <project> docs/specs/<spec>.md --reason "<why>"
saka spec draft <project> docs/specs/<spec>.md --idea "<idea>" --wait   # the architect drafts a spec that passes lint
saka draft list <project>                             # also: show, timeline, steer, interrupt, stop, retry, cancel
saka task list
saka task steer <task> "<message>"                     # also: interrupt, stop --reason, timeline
saka permission answer <request> allow --remember      # allow the same request in this project from now on
```

A project is a folder in a workspace repo. Its `saka.toml` names the project and binds the code repos its tasks change. Code repos do not have to be the workspace repo, so docs can live in a repo of their own:

```toml
[project]
name = "shop"

[[repo]]
name = "api"                                    # lowercase letters, digits, and -; at most 32 characters
url = "https://github.com/acme/shop"
default = true                                  # exactly one default when there is more than one repo
base_branch = "main"                            # optional
path_scope = ["services/api/**", "pnpm-lock.yaml", "!services/api/fixtures/**"]   # optional, default ["**"]

[[repo]]
name = "web"
url = "https://github.com/acme/web"
```

A project binds 1 to 16 repos. Each binding has its own base branch for worktrees, `.saka/environment.toml`, pull requests, and merge checks. Without `base_branch`, the workspace repo uses the project's base branch and any other repo uses its default branch. Saka re-reads `saka.toml` at the tip of the workspace base branch every `[workspace] config_poll_secs` and on `saka project reload <project>`. An invalid file, or one that drops a repo that still has live tasks, is refused and shown as the project's `config_error`, and the old bindings stay in effect.

A spec's Steps table can name each step's repo in an optional `Repo` column; an empty cell means the default repo. Approve refuses a name the project does not bind (`step_repo_unknown`). A task may only change paths inside its binding's `path_scope`, plus the project folder when the repo is the workspace repo. Patterns are globs: `*` stays within one directory, `**` crosses directories, and a pattern starting with `!` excludes. Saka checks the branch's diff against its base before it pushes and again before it merges. A diff outside the scope first gets a follow-up turn, then blocks the task with `path_scope_violation`.

A spec's context is pinned with it. Saka reads two kinds of markdown doc at the same commit as the spec and stores them with each revision:

- docs the spec links to with ordinary markdown links;
- docs in the project folder whose frontmatter says `context: always`.

A changed doc makes a new revision. Approving it again repins the steps that have not started. A link to a missing doc, or context over 32 docs or 256 KiB, is a lint error that blocks approve. Each task's first turn puts these documents first, the spec as document 1 and its context after it. The step, its acceptance criteria, its Done when commands, and the environment's ports, toolchains, and session secret names follow. `GET /api/v1/work-items/<id>/work-packet` returns that turn exactly as the agent received it.

Saka polls each open pull request, so it needs no inbound connection. It keeps polling while the task is blocked or the agent is still working, so a pull request merged on GitHub at any point ends the session, closes the blocker, and finishes the task. `saka_step_advance_seconds` measures how long the next step waits after the merge before it is admitted. To react to CI sooner, set `[hooks] listen` and give the integration a `webhook_secret_file`. Then point the GitHub App webhook at `https://<public host>/hooks/<scm id>` through a reverse proxy or tunnel. Subscribe it to pull requests, check runs, check suites, and statuses. That listener serves only `POST /hooks/{id}`, and the control API stays on loopback. A delivery must carry a valid `X-Hub-Signature-256`, and each delivery ID is handled once. A delivery never changes state by itself: it makes Saka read the pull request again from the API.

Before a step's pull request can merge, its gate must pass at the pull request's head. The repo picks the gate's sources in `[gate]` of `.saka/environment.toml`, which Saka always reads from the default branch:

- `scm`: CI that reports to GitHub;
- `saka`: Saka runs the step's Done when commands, then any `[gate] commands`, in the task's environment;
- `both`: both must pass;
- `auto` (the default): Saka's commands must pass, and CI counts once it reports.

```toml
[gate]
source = "auto"             # auto | scm | saka | both
commands = ["make lint"]    # run after the step's Done when commands
required_checks = ["test"]  # CI checks that must report and pass
timeout_minutes = 30        # per command
```

Saka reports its result on the pull request as the `saka/verify` status when the GitHub App may write commit statuses. A failing command blocks the task with `ci_red`, and a retry hands the agent the failed command and the last lines of its output.

A task is done only when its merge is proven. If every required gate source passed at the pull request's head that was merged, the task finishes right away. If not, for example because the pull request was merged on GitHub before its gate passed, Saka checks the merge commit with the same sources: it runs the Done when and gate commands in the task's worktree at that commit, and reads CI on that commit. A failure blocks the task with `dod_failed`, and the next step waits. After you fix the base branch, `retry` checks the tip of the base branch. `saka_dod_verifications_total{outcome}` counts proven, passed, and failed checks.

Saka retries some blockers by itself, at most twice per stage, after 1 and then 5 minutes. These are blockers a person has nothing to decide on: a lost session, an unreachable SCM, an environment that failed for reasons other than its setup commands, a head that moved during merge, or a gate runner that crashed. A manual `retry` needs a reason and is allowed three times per stage. After that, only `cancel` remains.

Configuration lives in `$XDG_CONFIG_HOME/saka/config.toml` (or the file named by `SAKA_CONFIG`); [`docs/config.example.toml`](docs/config.example.toml) lists every field with its default. Saka needs a GitHub App installed on the repository (see [End-to-end run](#end-to-end-run) for the permissions) and a Claude Code account directory that is already logged in.

## Development

| Path | Contents |
|---|---|
| `crates/` | The product crates; the allowed dependencies between them are checked by `cargo xtask deps` |
| `web/` | The web app (Vite, React), embedded in the binary with `--features embed-web` |
| `xtask/` | Repository tasks: `check`, `deps`, `hygiene`, `wasm`, `test-systemd`, `test-claude-memory`, `e2e` |
| `spikes/` | Research experiments, excluded from every check |

```sh
make check               # format, lint, tests, saka-wasm bindings, dependency direction, source hygiene, API drift, web checks
cargo xtask wasm         # builds the spec lint for the browser into web/src/wasm/pkg (needed before pnpm dev, test, or build)
make test-systemd        # the session supervisor in a real user systemd unit
make test-claude-memory  # the installed claude CLI loads instructions only from the session worktree
make e2e                 # both of the above, then one real delivery loop (needs credentials, see below)
```

Every SCM adapter declares its capabilities, such as merge methods, webhook signature, and branch deletion. `saka serve` refuses a config that asks for one the adapter lacks. Each adapter also passes the shared conformance suite in `crates/saka-scm/tests/conformance.rs`, run as part of `make check` or alone with `cargo test -p saka-scm --test conformance`. A new adapter adds a `Harness` there.

The Rust toolchain is pinned in `rust-toolchain.toml`; Node and pnpm in `web/package.json` (run pnpm as `corepack pnpm` from `web/`). The web app lints specs with `saka-spec` compiled to WebAssembly; `cargo xtask wasm` installs the `wasm-bindgen` CLI matching `Cargo.lock` under `target/tools/` and writes the bindings to `web/src/wasm/pkg/`, which is not committed. On a shared machine `make` runs each target through `scripts/limited`; set `CI=true` to skip that.

## End-to-end run

`make e2e` drives one real loop: Claude Code writes the code, GitHub runs CI, and Saka merges. It is not part of CI and needs:

- a user systemd instance (`systemctl --user is-system-running` says `running` or `degraded`), `git`, and the `claude` CLI;
- a test repository you own, with a `docs/saka.toml` naming the repository itself, a Go module in `cli/`, and a CI workflow that runs on `pull_request` for any base branch;
- a GitHub App installed on that repository with these repository permissions: Contents read and write, Pull requests read and write, Checks read, Commit statuses read and write (for the `saka/verify` status), Metadata read (no webhook needed). Download a private key and `chmod 600` it;
- a Claude Code account directory that is already logged in: `CLAUDE_CONFIG_DIR=<dir> claude`, then `/login`.

| Variable | Value |
|---|---|
| `SAKA_E2E_GITHUB_APP_ID` | The App ID |
| `SAKA_E2E_GITHUB_PRIVATE_KEY_FILE` | Path to the private key (mode 0600) |
| `SAKA_E2E_REPO` | `<owner>/<name>` of the test repository |
| `SAKA_E2E_CLAUDE_CONFIG_DIR` | The logged-in Claude Code account directory |
| `SAKA_E2E_MODEL` | Optional; default `haiku` |

The run creates base branch `e2e/<run-id>` from the default branch. It commits a context doc (`docs/e2e-context-<run-id>.md`) and a two-step spec that links to it (`xtask/src/e2e/fixture-spec.md.tmpl`). It then starts `saka serve` with a config of its own, registers the project, approves the spec, and merges each step through the CLI once CI passes. When S1's session starts, the run reads S1's work packet and checks two things: the packet carries the pinned context doc, and the doc's marker line sits inside that document. While S1 runs, the run steers its session, interrupts it and steers it again, kills the runner unit so the session resumes on its own, and restarts `saka serve` while the resumed session runs; at the end it checks that S1 has a resumed session and a timeline with the steer and the result. A permission request whose command is one of the spec's Done when commands, or `sleep 30` (asked for by the scenario), is allowed; any other request is denied with a hint. The run must finish within 30 minutes with the spec shipped, two merged pull requests, and their task branches deleted, and S2 must be admitted within 5 minutes of S1's merge. The run then asks the architect to draft a new spec from a short idea (`saka spec draft … --wait`) and checks that the draft succeeds, its commit is the latest change of that file on the base branch, the spec is registered with origin `architect`, and the architect session used no tool outside its allow-list. After that the base branch is deleted. When a run fails, the base branch is kept for inspection, and the server log is in `target/e2e/<run-id>/serve.log`.

## Spikes

Throwaway experiments from the research phase. Each folder answers one question; the measured results are recorded in the research notes, not here.

| Folder | Question | Run |
|---|---|---|
| `spikes/s1-agent` | Can our own runtime drive Claude Code and Codex headless: stream, steer, interrupt, kill and resume, approvals? | `cargo run -p s1-agent -- claude 20` (expects a bare remote at `.dev/s1/origin.git`) |
| `spikes/s2-editor` | Is a CodeMirror 6 live-preview editor lossless on a real vault, and does it resolve links like Obsidian? | `cargo run -p s2-editor -- <vault-copy> spikes/s2-editor/web/dist`, then `pnpm exec node tests/drive.mjs roundtrip` in `web/` |
| `spikes/s4-spec` | How many existing specs fit the task model as written, and which markdown parser to use? | `cargo run --release -p s4-spec -- specs <vault> <project>...` |
| `spikes/s5-toolchain` | Can every repository run with its own Go, Node, and pnpm versions without a version manager? | `cargo run -p s5-toolchain -- spikes/s5-toolchain/plan.json round1` (edit the example repos first) |

Toolchains are pinned in the repository: Rust in `rust-toolchain.toml`, Node and pnpm in `spikes/s2-editor/web/package.json` (`devEngines.runtime`, `packageManager`). `scripts/limited` runs a command in its own cgroup, so builds and agent sessions cannot starve a shared machine. Local data goes to `.dev/`, which is ignored.

## License

Apache-2.0. See [LICENSE](LICENSE).
