# Saka guide

How to build and run Saka, how it behaves, and how to work on it.

## Running

```sh
cargo build --release -p saka --features embed-web   # after `cargo xtask wasm` and `corepack pnpm build` in web/
saka serve                                            # API, workers, and the web app on 127.0.0.1:7420
cd <your clone> && saka init                          # writes docs/saka.toml and checks it; exit 5 until it is pushed
git add docs/saka.toml && git commit -m "chore: set up Saka" && git push
saka init                                             # sees the pushed saka.toml and registers the project
saka spec template > docs/specs/<spec>.md            # a spec that passes lint (--locale id for Indonesian)
saka spec approve <project> docs/specs/<spec>.md       # again after an edit: repins steps that have not started
saka spec withdraw <project> docs/specs/<spec>.md --reason "<why>"
saka spec draft <project> docs/specs/<spec>.md --idea "<idea>" --wait   # the architect drafts a spec that passes lint
saka draft list <project>                             # also: show, timeline, steer, interrupt, stop, retry, cancel
saka task list
saka task steer <task> "<message>"                     # also: interrupt, stop --reason, timeline
saka permission answer <request> allow --remember      # allow the same request in this project from now on
saka inbox list [--project <project>]                  # what waits for you across projects, newest first
saka inbox dismiss <item> --reason "<why>"             # only a spec approval; it returns with the spec's next revision
```

The web app opens on the Inbox: one card per permission, blocker, spec approval, step start, or merge that waits for you, with a badge in the header. Each card has one main action that calls the same endpoint as the task page. A `notify` hook with `on = "inbox_item_opened"` sends a message when an item starts waiting; with `[notify] public_url` set it links to `/inbox/<id>`.

A dev, fix, or architect session can ask you a question when a choice is yours to make: it calls the `request_decision` tool of `saka mcp` with two or three options, the first being its recommendation, and ends its turn. The question waits as a card in the Inbox and the session stays parked in `waiting`: nothing is pushed and the idle timeout does not run. Your answer reaches the same conversation as its next turn, also after a restart or when the runner was lost (the session is resumed for it). Stopping, cancelling, or taking over the task closes the question. A question that waits longer than `agent.decision_wait_hours` (72 by default) blocks the task with `decision_expired`; a retry tells the agent to go on with the recommended option and to say so in the pull request. An agent may ask one question at a time and at most five per stage run; reviewer sessions never get the tool.

`saka init` runs inside a local clone of a code repo. It asks where the docs live (this repo, or the local clone of a separate docs repo), writes `saka.toml` there or adds this repo to an existing one without touching its comments, and reports the Go, Node, pnpm, Rust, and Python versions the repo pins. For a version that is not exact, it offers a version file (`.nvmrc`, `.python-version`, `rust-toolchain.toml`) or prints the official command (`npm pkg set …`, `go mod edit …`); `package.json` and `go.mod` are never edited. It also offers a `.saka/environment.toml` with setup commands from the root lockfiles. Then it checks the file with the server (`POST /api/v1/projects/actions/check-config`): integration, App access, and base branches, all problems at once. Saka never commits, pushes, or fetches in your repo; once the same `saka.toml` is on the remote base branch, the next `saka init` registers the project, or reloads it when it is already registered. Every choice is a flag (`--docs repo --docs-repo <dir> --docs-path <folder>`, `--project-name`, `--repo-name`, `--base-branch`, `--path-scope`, `--default`, `--write-version-files`, `--write-recipe`, `--no-register`, `--dry-run`, `--yes`, `--json`). Exit codes: 0 done, 1 failed, 2 usage or environment, 3 config problems, 5 waiting for a push. `saka project add <url> --path <folder>` still registers a project whose `saka.toml` is already pushed.

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

An optional `[workflow]` table in the same file says how the project works. `preset` is `full_loop` (the default: approved specs become tasks that Saka builds, reviews, and merges), `build_only` (Saka builds and opens the pull request; people merge it), `review_only` (pull requests opened outside Saka get a review), or `custom`. A `[workflow.change_request]` table sets the trigger (`auto` or `manual`) and filters (`labels`, `authors`, `branches`, `paths`, `include_forks`, `include_drafts`) for outside pull requests, and `[workflow.stages]` sets the trigger of each stage. Saka refuses combinations that mean nothing, such as a fix stage without a review stage, and shows the line. Task approval and the merge action follow the workflow now.

With a `[workflow.change_request]` table, Saka watches the repo's open pull requests (by webhook and by polling). A pull request that passes the filters becomes a task that waits for the reviewer: Saka queues a review request at its current head, on its own (`auto`) or when someone with write access comments `/saka review` or adds the `trigger_label`, or when you press Review in Delivery or run `saka review <number> [--project <id or name>] [--repo <name>]`. Pull requests from forks and drafts are skipped unless the entry allows them, and Saka's own pull requests are never taken. A review request for a pull request that names no spec step (`<slug>/s<n>` in its title or description) is marked "No spec". The review itself arrives in a later change.

When a review asks for changes, Saka can answer it with fix rounds, at most `fix_rounds` per task (`[workflow] fix_rounds`, default 2). A task made from a spec step starts a round by itself when the workflow's `fix` stage is `auto` (the default of `full_loop`); a merge conflict in the queue starts one too. A pull request from outside gets a round only when its branch is in the same repo, never from a fork, and only when the `fix` stage is `auto`, someone with write access comments `/saka fix`, or you press Fix in Delivery or run `saka task fix <id>`. Each round pushes to the pull request's own branch with a lease on the reviewed head, and the new head is reviewed again.

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

Configuration lives in `$XDG_CONFIG_HOME/saka/config.toml` (or the file named by `SAKA_CONFIG`); [`docs/config.example.toml`](config.example.toml) lists every field with its default. Saka needs a GitHub App installed on the repository (see [End-to-end run](#end-to-end-run) for the permissions) and a Claude Code account directory that is logged in.

`saka scm create-app` creates the GitHub App for a new instance and adds it to the config. Run it on the host of `saka serve`: it never talks to the server. It opens your browser on a page served only on `127.0.0.1`, which posts an App manifest to GitHub with the permissions the loop needs (Contents and Pull requests write, Checks and Metadata read, Commit statuses write). After you confirm on GitHub, it exchanges the one-time code, writes the private key (and the webhook secret) as 0600 files in `<config dir>/scm/<id>/`, and appends an `[[scm]]` block to `config.toml` without touching the rest of the file. GitHub's client secret is never stored. Use `--org <login>` for an organization App, `--web-url https://<host>` for GitHub Enterprise Server, `--webhook-base-url https://<public url>` to turn on the webhook (it also adds `[hooks]` when the config has none), and `--no-browser` to open the printed URL yourself, for example through SSH port forwarding on a headless host. Then install the App, turn on **Enable Device Flow** in its settings (a manifest cannot), restart `saka serve`, and run `saka doctor`; the command prints these links. Exit codes: 0 created, 1 failed after the browser step (errors name the App settings URL when GitHub already created it), 2 refused before anything started, 130 interrupted. For an App you already have, download a private key, `chmod 600` it, and write the `[[scm]]` block by hand as in `docs/config.example.toml`.

`saka agent login` logs the `claude` CLI in to Saka's own account directory (`agent.account_dir`, by default `<data_dir>/agents/claude/default`, mode 0700). It runs `claude auth login` with `CLAUDE_CONFIG_DIR` set to that directory and only a short list of terminal and display variables, so your personal `~/.claude` and any `ANTHROPIC_API_KEY` in your shell never reach it. Add `--console` to log in with an Anthropic Console account. Saka never reads the credentials; `saka agent status` asks the CLI whether the account is logged in.

`saka doctor` checks whether the instance can run the loop: the server, migrations, the user systemd manager, the agent CLI and its login, quota, and the toolchain cache and sources. It prints one line per check with a stable id, `pass`, `warn`, `fail`, or `skip`, a code, and the command that fixes it. `--json` prints the full report, `--check <id>` runs only some checks, and `--strict` makes warnings fail too. It exits 0 without failures, 1 with one, and 2 when the config or a flag is refused. When the server is down, it runs the local checks itself from the config, without migrating the database. The server's report is `GET /api/v1/diagnostics`, and `saka_diagnostic_checks_total{id,status}` counts results.

Linking your GitHub account to an integration lets Saka author agent work as you. In the GitHub App settings, turn on **Enable Device Flow** and keep user-token expiration on. Then copy the App's **Client ID** (not the App ID) into `[[scm]] client_id`. `saka scm link` prints a code to enter at `https://github.com/login/device` and waits until you approve. `saka scm status` shows the linked login and when its refresh token expires, and `saka scm unlink` removes the link. Saka stores the short-lived user tokens encrypted and refreshes them without a client secret. To revoke them, use GitHub's settings → Applications. `saka_user_link_authorizations_total{integration,state}` and `saka_user_link_refresh_total{integration,outcome}` count link attempts and token refreshes. `[git] name` and `email` are Saka's own identity: the committer of every agent commit, and its author in bot mode.

`saka project authorship <project> --mode bot|user|user-pat` picks who authors a project's agent commits and opens its pull requests. `bot` is Saka and the App. `user` is the linked account, and the App badge still shows on its pull requests. `user-pat` is the owner of a fine-grained personal access token stored for the project, and its pull requests carry no badge. Store the token with `saka project user-token set <project>`, which reads one line from stdin. The token must start with `github_pat_`, all repos of the project must be on one integration, and the token needs read access to every one of them and write access to their pull requests. Saka checks the token with GitHub when you store it and reads its expiry from GitHub's answer. Only opening a pull request uses your token. Push, CI status, merge, and branch deletion always use the App. When the token expires or GitHub refuses it, the task stops with `user_token_invalid`, and storing a new token continues it by itself. `saka project user-token rm <project>` removes the token once the project uses another mode. `saka_change_requests_opened_total{credential}` counts pull requests by the credential that opened them.

## Signing in and remote access

By default Saka listens on loopback and asks for no credentials. Add an `[auth]` section to the config to open it beyond loopback, for example to use it from a phone:

```toml
[server]
listen = "0.0.0.0:7420"

[auth]
origin = "https://saka.example.com"   # the address people type; https, except http://localhost:<port> for development
rp_id = "example.com"                 # the domain of origin or a parent of it
```

**Saka does not terminate TLS.** Put a reverse proxy, a tunnel such as Cloudflare Tunnel or Tailscale Funnel, or a VPN in front of it, and use the `https` address it serves as `origin`. Without TLS in front, a device token travels in clear text, so do not open the listener to a network you do not trust. A non-loopback `listen` without `[auth]` exits with code 2 (`config_listen_not_loopback`).

With `[auth]` on, every request needs a credential, including one from loopback and one that arrives through your own proxy: a source address or a `Host` header proves nothing behind a proxy. Only `/healthz`, `/readyz`, `/api/v1/meta`, the web app files, and the sign-in endpoints answer without one. `/metrics` needs a credential too. The `Host` header must name the `origin` host or a loopback address, and a `POST` from a browser must come from `origin` or an entry of `server.allowed_origins`.

The operator is one person: there are no accounts or roles. A credential is either a device token or a session cookie.

A **device token** (`Authorization: Bearer sdt_…`) is for the CLI and other programs. `saka serve` writes a token for the local CLI to `<data_dir>/cli-token` with mode 0600 and replaces it on every start, so `saka` on the host works with no flag. The CLI reads that file only for a loopback server and refuses it when its mode lets anyone else read it (`token_file_mode`). For another machine, run `saka auth pair` on the host and `saka auth redeem <code> --name "<device>" --out <file>` on the device, then use `--token-file <file>` or `SAKA_TOKEN`.

A **session cookie** (`saka_session`, `HttpOnly; Secure; SameSite=Strict`) is for the browser and comes from a passkey. The first passkey needs a setup code from the host, so nobody who happens to open an exposed instance first can claim it. A session lasts `[auth] session_hours` (default 168, at most 720) from sign-in and is not renewed; signing out, revoking the passkey, or the end of that time ends it. Passkeys are verified by the `webauthn-rs` library; Saka does not implement WebAuthn itself.

```sh
saka auth setup                  # on the host: a code that registers a passkey, valid once for 15 minutes; also recovers a lost one
saka auth pair                   # a code that pairs another CLI or app (a token), valid once for 5 minutes
saka auth pair --passkey         # a code that registers a passkey from another browser
saka auth redeem <code> --name "CI box" --out ~/.config/saka/token
saka auth devices                # paired devices and passkeys
saka auth revoke <device-id>     # takes effect on the next request
saka auth revoke <passkey-id> --passkey   # also ends that passkey's sessions; the last passkey cannot be revoked
```

In the browser, open the `origin` address. With `[auth]` on and no passkey yet, Saka shows the setup page: run `saka auth setup` on the host, enter the code and a name for the device, and confirm the prompt of the browser or the phone. Later visits show the sign-in page; any page that answers `401` (for example after a session ends) leads there and returns to the page you asked for after you sign in. **Settings, Devices** (the Devices link in the navigation bar) lists passkeys and paired devices, revokes either one, adds a passkey to the browser you are signed in on, and creates a pairing code to register another browser or pair a CLI or app. A pairing code is shown once. Sign-out is the button next to it.

A pairing code is ten characters of Crockford base32 (`I`, `L`, `O` and `U` are never used; typing them as `1` or `0` works), is stored only as a hash, and works once. Five wrong guesses void every live code of the same kind. After ten failed attempts in a minute from the same address, the sign-in endpoints answer `429` with `Retry-After`; the address is the direct peer, so `X-Forwarded-For` is ignored. A token, a code, and a cookie are never written to logs, events, or error messages.

## Development

| Path | Contents |
|---|---|
| `crates/` | The product crates; the allowed dependencies between them are checked by `cargo xtask deps` |
| `web/` | The web app (Vite, React), embedded in the binary with `--features embed-web` |
| `xtask/` | Repository tasks: `check`, `deps`, `hygiene`, `wasm`, `test-systemd`, `test-claude-memory`, `test-claude-mcp`, `e2e` |
| `spikes/` | Research experiments, excluded from every check |

```sh
make check               # format, lint, tests, saka-wasm bindings, dependency direction, source hygiene, API drift, web checks
cargo xtask wasm         # builds the spec lint for the browser into web/src/wasm/pkg (needed before pnpm dev, test, or build)
make test-systemd        # the session supervisor in a real user systemd unit
make test-claude-memory  # the installed claude CLI loads instructions only from the session worktree
make test-claude-mcp     # the installed claude CLI sees and calls the `request_decision` tool of `saka mcp`
make e2e                 # all three of the above, then one real delivery loop (needs credentials, see below)
```

Every SCM adapter declares its capabilities, such as merge methods, webhook signature, and branch deletion. `saka serve` refuses a config that asks for one the adapter lacks. Each adapter also passes the shared conformance suite in `crates/saka-scm/tests/conformance.rs`, run as part of `make check` or alone with `cargo test -p saka-scm --test conformance`. A new adapter adds a `Harness` there.

The Rust toolchain is pinned in `rust-toolchain.toml`; Node and pnpm in `web/package.json` (run pnpm as `corepack pnpm` from `web/`). The web app lints specs with `saka-spec` compiled to WebAssembly; `cargo xtask wasm` installs the `wasm-bindgen` CLI matching `Cargo.lock` under `target/tools/` and writes the bindings to `web/src/wasm/pkg/`, which is not committed. On a shared machine `make` runs each target through `scripts/limited`; set `CI=true` to skip that.

## End-to-end run

`make e2e` drives one real loop: Claude Code writes the code, GitHub runs CI, and Saka merges. It is not part of CI and needs:

- a user systemd instance (`systemctl --user is-system-running` says `running` or `degraded`), `git`, and the `claude` CLI;
- a test repository you own, with a `docs/saka.toml` naming the repository itself, a Go module in `cli/`, and a CI workflow that runs on `pull_request` for any base branch;
- a GitHub App installed on that repository with these repository permissions: Contents read and write, Pull requests read and write, Checks read, Commit statuses read and write (for the `saka/verify` status), Metadata read (no webhook needed). Download a private key and `chmod 600` it;
- a Claude Code account directory that is logged in: point `agent.account_dir` at it and run `saka agent login`.

| Variable | Value |
|---|---|
| `SAKA_E2E_GITHUB_APP_ID` | The App ID |
| `SAKA_E2E_GITHUB_PRIVATE_KEY_FILE` | Path to the private key (mode 0600) |
| `SAKA_E2E_REPO` | `<owner>/<name>` of the test repository |
| `SAKA_E2E_CLAUDE_CONFIG_DIR` | The logged-in Claude Code account directory |
| `SAKA_E2E_MODEL` | Optional; default `haiku` |
| `SAKA_E2E_AUTHOR_TOKEN_FILE` | Optional; a token of an account other than the App that can push to the test repository. Default: `gh auth token` |

The run creates base branch `e2e/<run-id>` from the default branch. It commits a context doc (`docs/e2e-context-<run-id>.md`), a two-step spec that links to it (`xtask/src/e2e/fixture-spec.md.tmpl`), and a second project folder (`docs-review/saka.toml`, preset `review_only`, limited to this run's outside branch). It then starts `saka serve` with a config of its own, registers the project, and approves the spec. The config makes the repo `autonomous` in `[autonomy]`, so steps start on their own and Saka merges each pull request when the review approved its head and CI is green on it. The run never taps merge. A permission request whose command is one of the spec's Done when commands, or `sleep 30` (asked for by the scenario), is allowed; any other request is denied with a hint.

The spec run checks:

- **Runtime.** When S1's session starts, the run reads S1's work packet: the packet carries the pinned context doc, and the doc's marker line sits inside that document. While S1 runs, the run steers its session, interrupts it and steers it again, kills the runner unit so the session resumes on its own, and restarts `saka serve` while the resumed session runs. At the end S1 has a resumed session and a timeline with the steer and the result.
- **Review and automatic merge.** Every task reports `merge_auto`, `saka_merge_requests_total` counts merges by `autonomy` and none by an operator, and each pull request is merged. The reviewer posts on Saka's own pull requests (as comments, since the provider refuses a verdict on one's own pull request).
- **Fix round.** When S2's session starts, the run interrupts it and steers it to write a placeholder instead of quoting S1, so AC-02 is plainly unmet. The reviewer must answer `changes_requested` at that head, a fix round must run and push, a review must follow at the new head with `approved`, and GitHub must show Saka's review at both heads. If the reviewer approves the first head anyway, the run fails with "the fix path was not exercised"; it never passes without the round.
- **Touches.** The run counts the operator's decisions from approve to shipped (the spec approval, and any merge, retry, or start). More than 3 fails the run; it expects 1, because the repo is `autonomous`. The count is printed. The tool permissions the agent asks for are policy, not decisions: they are answered by the run's own rules and listed beside the count, not in it, and so are the scenario's injections (steer, interrupt, kill, restart, `sleep 30`).
- **Finish.** The spec ships within 40 minutes with two merged pull requests and their task branches deleted, and S2 is admitted within 5 minutes of S1's merge.

After the spec, the run proves `review_only` for a pull request from outside. It registers the second project, opens a pull request from a plain branch (`e2e-ext/<run-id>`) as the account behind `gh auth token` (or the token in `SAKA_E2E_AUTHOR_TOKEN_FILE`), because the provider refuses a verdict from the account that opened the pull request. Saka takes it as an `external_change` with no spec, the reviewer posts a native review at the head, and the run checks on GitHub that the review of the App exists at that sha with the state the verdict maps to. Saka must not merge it: once the pull request's checks finish, it is still open, unmerged, with no merge action and no automatic merge. Then the run closes it.

The run then asks the architect to draft a new spec from a short idea (`saka spec draft … --wait`) and checks that the draft succeeds, its commit is the latest change of that file on the base branch, the spec is registered with origin `architect`, and the architect session used no tool outside its allow-list. After that the base branch is deleted. When a run fails, the base branch is kept for inspection, and the server log is in `target/e2e/<run-id>/serve.log`.

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

Apache-2.0. See [LICENSE](../LICENSE).
