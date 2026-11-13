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
saka spec template --kind bugfix                       # one template per kind: feature, bugfix, refactor, chore, security_patch
saka spec approve <project> docs/specs/<spec>.md       # again after an edit: repins steps that have not started
saka spec quick <project> --kind chore --title "<title>" --done-when "<command>"   # writes, commits, registers, and approves a small spec in one command
saka spec withdraw <project> docs/specs/<spec>.md --reason "<why>"
saka spec draft <project> docs/specs/<spec>.md --idea "<idea>" --wait   # the architect drafts a spec that passes lint
saka spec session <project> docs/specs/<spec>.md --idea "<idea>" --wait   # a conversation with the architect; --revise edits an existing spec
saka draft message <draft> "<reply>"                  # also: changes [--diff], commit [--message "<text>"], discard --reason "<why>"
saka draft list <project>                             # also: show, timeline, steer, interrupt, stop, retry, cancel
saka task list
saka task steer <task> "<message>"                     # also: interrupt, stop --reason, timeline
saka permission answer <request> allow --remember      # allow the same request in this project from now on
saka decision show <question>                          # the question and its options, numbered from 1 (1 is the agent's recommendation)
saka decision answer <question> --option 2 --note "<text>"   # also: list; an option, a note, or both
saka inbox list [--project <project>]                  # what waits for you across projects, newest first
saka inbox dismiss <item> --reason "<why>"             # only a spec approval; it returns with the spec's next revision
```

The web app opens on the Inbox: one card per permission, question, blocker, spec approval, step start, or merge that waits for you, with a badge in the header. Each card has one main action that calls the same endpoint as the task page. A `notify` hook with `on = "inbox_item_opened"` sends a message when an item starts waiting; with `[notify] public_url` set it links to `/inbox/<id>`.

A dev, fix, or architect session can ask you a question when a choice is yours to make (`GET /api/v1/decision-requests` lists them and `POST /api/v1/decision-requests/<id>/answer` answers one; the Inbox card calls the same endpoint): it calls the `request_decision` tool of `saka mcp` with two or three options, the first being its recommendation, and ends its turn. The question waits as a card in the Inbox (with its options, the agent's recommendation marked, and a note field; the task and draft pages list it too, answered or not) and the session stays parked in `waiting`: nothing is pushed and the idle timeout does not run. Your answer reaches the same conversation as its next turn, also after a restart or when the runner was lost (the session is resumed for it). Stopping, cancelling, or taking over the task closes the question. A question that waits longer than `agent.decision_wait_hours` (72 by default) blocks the task with `decision_expired`; a retry tells the agent to go on with the recommended option and to say so in the pull request. An agent may ask one question at a time and at most five per stage run; reviewer sessions never get the tool.

A conversation with the architect (`saka spec session`, or "Work as a conversation" in the new-draft dialog) differs from a one-shot draft in who ends it. After every turn Saka lints the target file and the draft waits in `awaiting_operator`: the draft page shows the architect's timeline and questions, the files it changed with their diffs, and the lint result, and nothing is committed. You reply with a message (`saka draft message`), answer a question, commit, or discard. Commit writes every changed markdown file in the project folder in one docs commit and registers the target spec with origin `architect`; it is refused (`409`) while a question is open, while the lint fails, when nothing changed (`draft_no_changes`), or when a file the draft changed was also changed on the base branch (`draft_base_moved`, with the paths; files that only changed elsewhere do not block it). With `--revise` the architect edits a spec that already exists and the commit records a new revision of it that you approve the usual way; the approved revision and the steps that already run keep their pin. The architect writes only markdown files inside the project folder (never `saka.toml`, code, or anything through a symlink). A conversation nobody answers for `[architect] session_idle_hours` (72 by default, 1 to 168) fails with `session_idle_expired`; the agent's own idle timeout does not apply while it waits for you.

The Sessions area (`/sessions`) lists every agent session that is not over, across projects, with its task or draft, its role, its state, and how many permission requests and questions wait on it (`GET /api/v1/sessions?active=true`, optionally with `project_id`). Open one to read its activity as it happens, answer a waiting permission or question where it is shown, and steer, interrupt, or stop the agent; the buttons shown are the ones the session allows right now. On a screen narrower than 640 px the navigation is a bar at the bottom with Inbox (and its count), Docs, Delivery, and Sessions, and a tab that the phone froze in the background reads what it missed when you come back to it.

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

An optional `[workflow]` table in the same file says how the project works. `preset` is `full_loop` (the default: approved specs become tasks that Saka builds, reviews, and merges), `build_only` (Saka builds and opens the pull request; people merge it), `review_only` (pull requests opened outside Saka get a review, and a merge you ask for), or `custom`. A `[workflow.change_request]` table sets the trigger (`auto` or `manual`) and filters (`labels`, `authors`, `branches`, `paths`, `include_forks`, `include_drafts`) for outside pull requests, and `[workflow.stages]` sets the trigger of each stage. Saka refuses combinations that mean nothing, such as a fix stage without a review stage, and shows the line. Task approval and the merge action follow the workflow now.

With a `[workflow.change_request]` table, Saka watches the repo's open pull requests (by webhook and by polling). A pull request that passes the filters becomes a task that waits for the reviewer: Saka queues a review request at its current head, on its own (`auto`) or when someone with write access comments `/saka review` or adds the `trigger_label`, or when you press Review in Delivery or run `saka review <number> [--project <id or name>] [--repo <name>]`. Pull requests from forks and drafts are skipped unless the entry allows them, and Saka's own pull requests are never taken. A review request for a pull request that names no spec step (`<slug>/s<n>` in its title or description) is marked "No spec". The review itself arrives in a later change.

A spec says what kind of work it is with a `kind` line in its frontmatter: `feature` (the default when the line is missing), `bugfix`, `refactor`, `chore`, or `security_patch`. Any other value is a lint error (`kind_invalid`). The kind decides the safeguards of the task, and each kind has its own template in English and Indonesian: `saka spec template --kind bugfix --locale id`, or the kind picker of "New spec" in the Docs area. The `feature` template has a Steps table; the other templates are one `## Done when` list. A `bugfix` also needs a `## Regression` section, a list of 1 to 10 items that each hold one command in backticks, the commands that show the bug; without it the spec is not ready (`regression_missing`). Another kind with that section only gets a warning. A bugfix is one change: a bugfix spec with several steps is not ready (`bugfix_multi_step`); split the work or use `feature` or `refactor`.

The kind also picks the workflow of the task. A `security_patch` always has a review stage, even in a `build_only` project, and always runs at the strict level: Saka asks you to start each step and to approve the merge, whatever `saka.toml` or the operator ceiling says. A project can tune the other kinds, and only narrow or add safeguards:

```toml
[workflow.kinds.bugfix]
stages = { review = "auto", merge = "manual" }   # each stage: auto | manual | off
autonomy = "standard"                            # only lowers the level of the repo and the operator
test_paths = ["src/**/fixtures/**"]              # bugfix only: extra patterns that mark test files

[workflow.kinds.chore]
stages = { review = "off" }                      # only when the operator allows it, see below
```

A kind may change the trigger of a stage the workflow has and may add a `review` or `fix` stage; it cannot add another stage, turn `dev` off (`kind_stage_required`), or turn `review` off (`kind_review_required`), except for a `chore`. A chore skips review only when the operator sets `[workflow] allow_unreviewed_chore = true` in `config.toml` (the default is `false`; without it the file is refused with `kind_review_not_allowed`, and a task never skips review while it is `false`). A chore without a review has no fix stage either. The gate (`[gate]` and the Done when commands) never changes with the kind. A task keeps the kind of the revision it is pinned to, and a reload of `saka.toml` changes the workflow of the tasks that have not started. The project's Settings page lists the overrides and whether the operator allows unreviewed chores.

A quick task writes a small spec and starts it in one action: pick a kind, give a title, an optional description, the Done when commands (1 to 10), the Regression commands (a bugfix needs 1 to 10, other kinds none), optional acceptance criteria, and for a project with several repos the repo the step changes. Saka renders the spec from those fields (the same bytes for the same input, in English or Indonesian), refuses it before writing anything when it does not lint, commits it to the base branch as `<project folder>/specs/<kind>-<title>.md` (no kind in front for a `feature`, or the `path` you give), registers it with origin `quick`, and approves it with the reason `quick task`. Titles, descriptions, and criteria are written as plain text: line-start `#`, `---`, `>`, `|`, fences, and list markers are escaped, and a command that holds backticks gets a longer fence, so no input can add a heading, a table, or another section. Saka then checks that the parsed spec holds exactly the input and answers `quick_spec_render_mismatch` without committing when it does not. At the strict level (always for a `security_patch`) the task still waits for you to start each step, and the web button reads "Create and approve" instead of "Create and start". `saka spec quick <project> --kind bugfix --title "Login redirect loop" --done-when "make test" --regression "cargo test --test login"` prints the spec, its path, the commit, and the task key; `--create-only` stops before the approval, `--json` prints one document, and `--path`, `--description`, `--ac`, `--repo`, and `--locale` are optional. The "Quick task" button in the Docs and Delivery areas opens the same form with a preview of the file and the buttons "Create and start" and "Create only". The API is `POST /api/v1/projects/{id}/quick-specs` with an `Idempotency-Key`. A file or slug that exists is refused (`file_path_conflict`, `spec_slug_conflict`), a spec that does not lint is refused with its issues (`spec_not_ready`), and a failed approval after the commit answers `quick_spec_not_approved` with the registered spec: the file stays, and you approve it after fixing it. CLI exit codes: 0 done, 1 refused or failed lint, 2 bad arguments, 4 conflict.

When a review asks for changes, Saka can answer it with fix rounds, at most `fix_rounds` per task (`[workflow] fix_rounds`, default 2). A task made from a spec step starts a round by itself when the workflow's `fix` stage is `auto` (the default of `full_loop`); a merge conflict in the queue starts one too. A pull request from outside gets a round only when its branch is in the same repo, never from a fork, and only when the `fix` stage is `auto`, someone with write access comments `/saka fix`, or you press Fix in Delivery or run `saka task fix <id>`. Each round pushes to the pull request's own branch with a lease on the reviewed head, and the new head is reviewed again.

In the `review_only` preset an outside pull request whose head the review approved moves to the merge stage (`merge` is `manual`, and Saka never merges an outside pull request by itself). Once the checks of that head pass, someone presses Merge in Delivery or runs `saka task merge <id>`, and the pull request waits in the same per-branch queue as the tasks of Saka. Saka never rebases or force-pushes a branch that is not its own, and never deletes it after the merge. When the pull request is behind its base branch, the task blocks with `change_behind_base`. On GitHub, for a pull request that is not from a fork, the primary action "Update branch" (Delivery, or `POST /api/v1/work-items/{id}/actions/primary`) asks the provider to merge the base into the branch; the new head is reviewed again and the merge goes on by itself once it is approved and the checks pass. Where Saka cannot update the branch (a fork, or GitLab), it comments on the pull request and asks the author to update it. The checks of an outside pull request come from the provider only; Saka does not run the Done when commands on it.

A comment with `/saka merge` at the start of a line, from someone with write access, asks for the merge of a pull request of Saka and of an outside pull request alike, with the same effect as Merge in Delivery (the reason is `comment by <login>`). Saka reads these comments on the open pull requests of its own tasks too, and only for this command; `/saka review` and `/saka fix` still work on outside pull requests only. When the request is refused (for example the review has not approved the current head), the task does not change and Saka answers with one comment that holds the reason and its code, once per head and code. A comment from someone without write access is ignored without an answer.

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

`saka scm create-app` creates the GitHub App for a new instance and adds it to the config. Run it on the host of `saka serve`: it never talks to the server. It opens your browser on a page served only on `127.0.0.1`, which posts an App manifest to GitHub with the permissions the loop needs (Contents, Issues and Pull requests write, Checks and Metadata read, Commit statuses write). After you confirm on GitHub, it exchanges the one-time code, writes the private key (and the webhook secret) as 0600 files in `<config dir>/scm/<id>/`, and appends an `[[scm]]` block to `config.toml` without touching the rest of the file. GitHub's client secret is never stored. Use `--org <login>` for an organization App, `--web-url https://<host>` for GitHub Enterprise Server, `--webhook-base-url https://<public url>` to turn on the webhook (it also adds `[hooks]` when the config has none), and `--no-browser` to open the printed URL yourself, for example through SSH port forwarding on a headless host. Then install the App, turn on **Enable Device Flow** in its settings (a manifest cannot), restart `saka serve`, and run `saka doctor`; the command prints these links. Exit codes: 0 created, 1 failed after the browser step (errors name the App settings URL when GitHub already created it), 2 refused before anything started, 130 interrupted. For an App you already have, download a private key, `chmod 600` it, and write the `[[scm]]` block by hand as in `docs/config.example.toml`.

`saka scm connect-gitlab --token-file <path>` connects a GitLab host and adds it to the config. Create a project, group, or personal access token with the `api` scope, save it in a file with `chmod 600`, and run the command on the host of `saka serve`: it never talks to the server. It checks the token against GitLab (`GET /personal_access_tokens/self`) and refuses a revoked, expired, or rejected token or one without the `api` scope, then appends an `[[scm]]` block to `config.toml` without touching the rest of the file; a token that expires within 14 days only warns. Use `--web-url https://<host>` for a self-managed host (https only), `--api-url` when the API is elsewhere, `--id` to name the integration (default `gitlab`), and `--config <path>` to edit another config file. With `--webhook-secret-file <path>` it also names the webhook secret file: an existing file is kept, a missing one is created with 32 random bytes in hex (mode 0600), and `[hooks]` is added when the config has none. Then add the token's user to each project as a member (Developer to push and open merge requests, Maintainer to merge into protected branches and to let `saka doctor` read the project hooks), add a project webhook that posts to `https://<public host>/hooks/<id>` with the secret and the merge request, comment, pipeline, and job events, restart `saka serve`, and run `saka doctor --check scm.credential`. Exit codes: 0 added, 1 the token or GitLab failed the check, 2 refused before GitLab was called (a flag, a file, or an id already in the config). `saka init` prints this command for a GitLab host it does not know.

`saka agent login` logs the `claude` CLI in to Saka's own account directory (`agent.account_dir`, by default `<data_dir>/agents/claude/default`, mode 0700). It runs `claude auth login` with `CLAUDE_CONFIG_DIR` set to that directory and only a short list of terminal and display variables, so your personal `~/.claude` and any `ANTHROPIC_API_KEY` in your shell never reach it. Add `--console` to log in with an Anthropic Console account. Saka never reads the credentials; `saka agent status` asks the CLI whether the account is logged in.

`saka doctor` checks whether the instance can run the loop: the server, migrations, the user systemd manager, the agent CLI and its login, quota, the toolchain cache and sources, and each SCM integration (`scm.credential`, `scm.repo_access`, `scm.webhook` for a GitHub App, `scm.repo_webhook` for a GitLab project, `scm.user_link`). It prints one line per check with a stable id, `pass`, `warn`, `fail`, or `skip`, a code, and the command that fixes it. `--json` prints the full report, `--check <id>` runs only some checks, and `--strict` makes warnings fail too. It exits 0 without failures, 1 with one, and 2 when the config or a flag is refused. When the server is down, it runs the local checks itself from the config, without migrating the database. The server's report is `GET /api/v1/diagnostics`, and `saka_diagnostic_checks_total{id,status}` counts results.

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

With `[auth]` on, every request needs a credential, including one from loopback and one that arrives through your own proxy: a source address or a `Host` header proves nothing behind a proxy. Only `/healthz`, `/readyz`, `/api/v1/meta`, the web app files, and the sign-in endpoints answer without one. `/metrics` needs a credential too. The `Host` header must name the `origin` host or a loopback address, and a `POST` from a browser must come from `origin` or an entry of `server.allowed_origins`. Answers to a request from an `allowed_origins` entry carry CORS headers (and a preflight is answered), so a client app on another origin, such as the desktop app (`tauri://localhost`), can read them.

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

**From a phone.** Reach the instance through a tunnel or a VPN that gives it an `https` address (a reverse proxy with a certificate, Cloudflare Tunnel, Tailscale Funnel or `tailscale serve`), set `origin` to that address, and set `listen` to the address the proxy connects to. Open the address on the phone, enter a code from `saka auth setup` or from Settings, Devices on a browser you already signed in on, and confirm the prompt with the phone's fingerprint, face or PIN. Without TLS in front, browsers refuse to create a passkey and a device token travels in clear text.

Saka is installable as an app (a PWA): choose Install or Add to Home Screen in the browser. A service worker keeps the app files and nothing else, never an API answer, so what you see is always fresh; offline, the app opens and says it is offline. `saka doctor` reports an `auth.config` check: a skip while `[auth]` is off (a default loopback install is healthy, so `saka doctor --strict` stays green), a failure when it is on and no passkey is registered yet, a pass otherwise.

A pairing code is ten characters of Crockford base32 (`I`, `L`, `O` and `U` are never used; typing them as `1` or `0` works), is stored only as a hash, and works once. Five wrong guesses void every live code of the same kind. After ten failed attempts in a minute from the same address, the sign-in endpoints answer `429` with `Retry-After`; the address is the direct peer, so `X-Forwarded-For` is ignored. A token, a code, and a cookie are never written to logs, events, or error messages.

## Desktop app

The desktop app is the same web app in a native window (Tauri 2), with a tray icon, OS notifications, and a list of servers to switch between. There is no release yet: build it, or take the installers from the Desktop workflow.

**Install.** In GitHub, open Actions, choose the `desktop` workflow, and run it. It builds on Linux (`.deb` and `.AppImage`) and macOS (`.dmg`) and attaches the files to the run; it publishes nothing. The installers are not signed or notarized, so macOS asks you to confirm opening the app (right-click, Open). On Linux the app needs WebKitGTK 4.1 and a Secret Service keyring (GNOME Keyring or KWallet) to keep tokens.

**Build it yourself.** `make desktop-check` builds the shell and runs its tests; it needs the WebKit and GTK development libraries, so `make check` does not build it. `make desktop-bundle` builds the installers for the current system.

**Servers.** The first start shows the Servers page. Two kinds of server:

- *This computer* starts the bundled `saka serve` on a loopback port the app keeps, with its own config and data under the app data folder, no sign-in, and `tauri://localhost` as an allowed origin. It stops when you quit the app, and the tray offers Start and Stop; a server that dies is shown as disconnected until you start it again. Its log is `saka.log` in the `local` folder of the app data folder. Jobs and sessions use the same machinery as any Saka server, so the usual requirements (git, an agent CLI, systemd for runners) apply.
- A *remote server* is any Saka with `[auth]` on, reached over `https`; plain `http` is accepted only for `127.0.0.1`, `localhost` and `::1`. Add its address (no path), a name, and a pairing code from its Settings, Devices and sign-in page or `saka auth pair`. The app exchanges the code for a device token and keeps the token in the system keychain, never in a file. The remote server must list the app's origin: add `tauri://localhost` to `server.allowed_origins` in its config. If the token is revoked, the app asks for a new code.

The app talks to one server at a time and restarts when you switch, because the content security policy it runs under allows network access to the active server only. The app refuses a server whose API version is more than one behind its own, or that needs a newer app, and says so before making any other call.

**Notifications and tray.** The app checks the inbox every 30 seconds. When the number of waiting items goes up and the window is not focused, it raises a notification; focusing the window afterwards opens the new item (or the inbox when several arrived). Closing the window hides it to the tray; Quit in the tray menu exits.

**Updates.** The updater is off. It turns on only in a build made with a public key: set the repository variables `SAKA_UPDATER_PUBKEY` and `SAKA_UPDATER_ENDPOINT` (an `https` URL) and the secret `TAURI_SIGNING_PRIVATE_KEY` before running the workflow. No key is generated or stored by Saka, and without them the app never looks for updates.

## Bugfix test-first

A spec with `kind: bugfix` is done only when its test failed on the old code and passes on the new. The spec lists the commands that run the test in a `## Regression` section, one command in backticks per item, at most 10, and has one step (`## Done when`, not a `## Steps` table). The agent of a bugfix task works in two moves:

1. It writes a regression test, changes test files only, commits, and calls the `regression_check` tool. The tool is offered to the dev session of a bugfix task and to no other session. Saka answers at once ("recorded, end your turn") and runs the regression commands at that commit. At least one must fail there: that is the proof. The result reaches the agent as its next message.
2. Only after `reproduced` does it fix the bug. At the head, the gate runs the regression commands together with the Done when commands and they must pass, and the test commit must still be in the branch (an ancestor, or the same patch after a merge queue rebase). Otherwise the task stops with `ci_red` and the check `saka/regression-commit`.

Saka recognises test files by path: `**/tests/**`, `**/test/**`, `**/__tests__/**`, `**/spec/**`, `**/*_test.*`, `**/*.test.*`, `**/*.spec.*`, `**/test_*.py`, `**/*_spec.rb`, `**/*Test.java`, `**/*Tests.cs` and `**/*Test.kt`. A repo with another layout adds patterns in `saka.toml`:

```toml
[workflow.kinds.bugfix]
test_paths = ["src/**/fixtures/**"]
```

A check ends in one outcome. `reproduced`: a command failed. `not_reproduced`: every command passed, so the test does not catch the bug. `invalid_paths`: the commit changes a file that is not a test, and no command ran. `error`: a command timed out or Saka could not run it. A dirty worktree gets `dirty_worktree` and records nothing. Each check keeps the exit code, the duration and the last 8 KiB of redacted output of every command, shown in the **Regression evidence** panel of the task. A command that fails to compile or is mistyped also counts as a failure, which a machine cannot tell from the bug, so the reviewer of a bugfix receives the exit code and the output and returns `changes_requested` when the failure does not show the bug in the spec.

The agent has three checks per dev run. The third check that is not `reproduced`, or a dev session that ends without `reproduced`, blocks the task with `regression_not_reproduced`. Retry resets the branch of the task to the base it was created from, records the old head in the `delivery.regression.branch_reset` event, and starts a new dev run with the same instructions. A fix round after review does not ask for another check. Metrics: `saka_regression_checks_total{outcome}`.

## Issues become specs

A `[workflow.issue]` table in `saka.toml` lets Saka turn GitHub or GitLab issues into specs, for a workflow that takes specs (not `review_only`). Every field is optional:

```toml
[workflow.issue]
trigger = "label"              # label (default) | auto
trigger_label = "saka:intake"  # 1 to 50 characters, no comma
labels = []                    # extra labels the issue must all carry
exclude_labels = ["wontfix", "duplicate", "question"]
kind_labels = { bug = "bugfix", chore = "chore", refactor = "refactor", security = "security_patch" }
default_kind = "feature"
include_confidential = false   # GitLab confidential issues
max_open = 20                  # intakes waiting for approval, 1 to 100

[workflow.issue.bugfix]        # optional: the light lane for bugs
done_when = ["cargo test --locked"]
regression = ["cargo test --locked"]
```

Someone with write access starts an intake by adding the `trigger_label` to an issue, or by commenting `/saka intake` at the start of a line (not in a quote or a code block). With `trigger = "auto"`, an issue created by someone with write access also starts one. You can take an issue in yourself with Add issue in Delivery or `saka issue intake <project> <number> [--repo <name>]`.

**Only users with write access can start an intake.** Anyone can open an issue on a public repo, so Saka checks the write access of whoever added the label, wrote the comment, or created the issue. Everyone else is ignored without a word on the issue; Saka only logs `intake.issue.denied` and counts it in `saka_issue_intake_denied_total`. The issue text is untrusted: Saka redacts secrets, keeps at most 16 KiB, writes it into a quick spec as plain text that cannot add a section, a criterion, or a command, and hands it to the architect inside a fenced block marked as untrusted data. Comments written after the intake never reach the spec.

The labels pick the kind (`kind_labels`, the safest kind wins when several match, `default_kind` when none does). A `bugfix` issue with a `[workflow.issue.bugfix]` table becomes a quick bugfix spec with those commands. Every other issue goes through the architect, with the template of its kind. Both end in a spec waiting for your approval: Inbox shows it with a link to the issue, and nothing runs before you approve it. Issues from before you turned the entry on are not taken in; an edit of the issue after the intake does not change the spec.

Saka answers on the issue, once per milestone: accepted, the spec is ready for approval, approved, the first pull request, done, dismissed, failed, and when `max_open` is full. The comments carry no local path or secret, and a link to Saka only when `[notify] public_url` is set. The pull request of the last step of the spec says `Closes #<number>` (or `Closes <owner>/<repo>#<number>` when the step is in another repo), so the provider closes the issue when the code merges into the default branch. Saka also closes the issue itself, as completed, about a minute after the last step merges, so the issue closes even when the project delivers to another base branch; if the provider already closed it, Saka only records that. The `done` comment names the base branch. A reopened issue is not closed again. Closing the issue before approval withdraws the spec or cancels the draft. In Delivery, the group From issues lists each intake with its state, lane, kind, and spec or draft, with Retry (at most three times) and Dismiss. A failed intake also opens an Inbox item. Dismiss works until the spec is approved; after that, cancel its task.

```
saka issue list <project> [--state accepted,failed]
saka issue intake <project> <number> [--repo <name>]
saka issue dismiss <intake id> [--reason <text>]
saka issue retry <intake id>
```

Exit codes follow the rest of the CLI: 0 done, 1 failed, 4 refused (already taken in, already approved, nothing to retry), `--json` prints the response.

Intake needs the integration to read issues and write comments. For a GitHub App that is the Issues permission (read and write) and the Issues event; `saka scm create-app` asks for both, and an App you created earlier must accept them in its settings. `saka doctor` reports a missing permission or event. For GitLab, the project hook needs the issue events (`issues_events`) and comment events (`note_events`); `saka doctor` reports a missing `issues_events` when a project takes issues. Without a webhook Saka polls, so an intake starts within `poll_interval_secs`.

## Reverting a merged task

A task whose pull request merged can be pulled back with one action and a reason. Press Revert on the task in Delivery, run `saka task revert <id> --reason "<why>"`, or call `POST /api/v1/work-items/{id}/actions/revert` with `If-Match` and an `Idempotency-Key`. The reason is one line of 1 to 500 characters. Revert is offered for a task of a spec step or of an outside pull request that is `done` and whose code change merged. It is not offered for a revert task, for a task that already has a revert running or done, or for a merge made outside Saka.

Saka makes a new task with the key `<task key>-R<n>` and the branch `saka/revert/<key>`. It starts at once, at any autonomy level: the action is the approval. It goes in front of waiting spec steps for a slot, and it needs no agent session, no review, and no fix round. A job makes the revert commit in a temporary environment at the tip of the base branch, and Saka pushes it and opens the pull request `Revert "<title>"`, with the original pull request, the merge commit, and your reason in the body and no `Closes #n`. The commit is authored the way an agent commit is, so the project's authorship mode applies. From there the revert is an ordinary task: the CI of the code host must pass at its head, it waits in the queue of the base branch, and it merges by itself in an `autonomous` repo whose `merge` stage is `auto` and waits for your Merge in any other case. The sync stage and the `merged` and `task_done` hooks run as for any task; the hook context carries `revert_of` with the id and key of the reverted task.

Saka picks what to revert from the commits, not from the merge method in config, so merges made on the host and changes of the method both work. A merge commit is reverted against its first parent. A squash commit is reverted as one commit. A rebased pull request, whose commits sit one after the other in the base branch, is reverted as one commit that undoes the whole range. When the original head is no longer available, Saka treats the change as a squash if its merge commit is the only commit that names the pull request, and stops otherwise.

When the revert cannot be made, the task blocks with `revert_conflict` and a reason. `conflict` lists the files that later changes touched. `not_in_base` means the merge commit is no longer in the base branch, `commits_unknown` means Saka cannot tell which commits the pull request brought in, and `empty` means a later commit already pulled the change back; for `empty` the action is Cancel. For the others, take the task over, make the revert yourself, push `saka/revert/<key>`, and return it: Saka opens the pull request from that branch and goes on from the merge stage. Retry makes the revert again at the newest tip of the base branch. A task cancelled before it merges leaves the original task as it was.

When the revert merges, the original task stays `done` and shows `reverted_by` (Delivery, `GET /api/v1/work-items/{id}`, and `saka task show <id>`); the revert task shows `revert_of`. In the spec view the step carries a "reverted" mark. A spec that already shipped stays shipped. Reverting does not bring the step back: for that, see Redoing a cancelled step. `saka_reverts_total{outcome}` counts reverts that merged, hit a conflict, or were cancelled.

## Redoing a cancelled step

A spec that has a cancelled task stops: it admits no other task and never ships. A task ends cancelled when you cancel it, or when you revert it while a failed deployment holds it. Instead of withdrawing the spec and writing it again, redo the step. Press Redo step on the cancelled task in Delivery (the paused spec shows a banner "Spec paused: S2 cancelled" with Redo step and Withdraw spec), run `saka task redo <id> --reason "<why>" [--note "<text>"]`, or call `POST /api/v1/work-items/{id}/actions/redo` with `If-Match` and an `Idempotency-Key` and the body `{ "reason": "<why>", "note": "<text>" }`. The reason is 1 to 500 characters and the note, which is optional, is at most 2000.

Redo is offered for a task of a spec step that is `cancelled`, whose spec is `approved`, and that was not redone yet. It is not offered for a task that is done (revert it first), for an outside pull request, or for a revert task. Changing the spec and approving it again after the task was cancelled does not block redo: the new task uses the revision that is approved now. If the step is no longer in that revision the answer is `redo_step_removed`; a second redo of the same task is `already_redone`.

In one transaction the cancelled task becomes `superseded` (it no longer holds the spec) and Saka makes a new task for the same step and kind, pinned to the approved revision with the key `<key>.<n>` (`s2` becomes `s2.2`, then `s2.3`) and the branch `<branch>-<n>`. The tasks that waited on the cancelled one now wait on the new one. The new task goes through admission like any task, at the tip of the base branch, and in a `strict` repo it waits for Start. The spec admits again, and it ships when the new task is done.

The agent gets the usual work packet plus a "Previous attempt" section that Saka writes: the key and outcome of the earlier task (cancelled, or reverted), its pull request and merge commit, the revert pull request when there is one, the failed deployment if one held the task (environment, sha, phase, code and the commands that failed), and your reason and note. The output of the failed commands is untrusted text, so it is only in a fenced data block that follows the trusted text. A bugfix starts test first again: its regression test is proven from scratch and the proof of the earlier task is not reused. Both tasks show the link (`redo_of`, `redone_by`, `saka task show <id>`, and in the spec view each step lists its attempts in order).

## Deploying and verifying

A task is done when its change merged. With an environment configured, it is done when the change is also running and healthy. Environments live only in the operator config (`config.toml`); how to check one lives in the repo (`saka.toml`), reviewed with the code. Saka calls your deploy tool as it is: a script on this host, a command in a throwaway environment, or a signed webhook to your pipeline. It is not a deploy engine and it never rolls back through the tool: recovery is a revert or a fix.

```toml
[[deploy]]
project = "tasks"
environment = "staging"            # ^[a-z0-9-]{1,32}$, unique per project
repo = "app"                       # required when the project binds more than one repo
branch = "main"                    # default: the base branch of the repo
action = "run_host"                # run | run_host | webhook
command = "./deploy.sh staging"
target_url = "https://staging.example.com"
gate = true                        # default: a task waits for a healthy deployment
on_failure = "revert"              # required: revert | fix
verify_secrets = ["SMOKE_TOKEN"]   # project secrets for the probe and the checks

[[deploy]]
project = "tasks"
environment = "preview"
repo = "app"
action = "webhook"
url = "https://ci.example.com/hooks/deploy"
secret = "DEPLOY_SIGNING_KEY"      # signs the body with HMAC-SHA256
gate = false
on_failure = "fix"
```

```toml
[verify.staging]
ready = 'curl -fsS "$SAKA_DEPLOY_URL/version" | grep -qx "$SAKA_SHA"'
ready_timeout_secs = 600           # 10..=3600, default 300
ready_interval_secs = 15           # 5..=300, default 15
commands = ["npm --prefix e2e run smoke"]   # up to 10, one line each
timeout_secs = 600                 # per command, default 300
```

Every merge of a code change to the environment's branch queues a deployment. An environment has one deployment running and at most one waiting: a merge that arrives while one waits joins it, and the sha is the branch tip read when the deployment starts, so a late or out-of-order merge never deploys an older commit. Then Saka runs the deploy action and, if `[verify.<environment>]` has a `ready` command, runs it every `ready_interval_secs` until it succeeds or `ready_timeout_secs` passes. That is how a webhook to an asynchronous pipeline waits until the target reports the new version. Then it runs every check command in a throwaway environment checked out at the deployed sha, with the repo's recipe and toolchain, and runs all of them even when one fails so the evidence is complete. An environment without `[verify.<environment>]` passes when the deploy action does, and `saka doctor` warns (`deploy.verify_missing`; `deploy.ready_missing` for a webhook without a probe).

The deploy action, the probe and the checks get `SAKA_DEPLOY_ID`, `SAKA_DEPLOY_ENV`, `SAKA_DEPLOY_URL` (when `target_url` is set), `SAKA_SHA`, `SAKA_PREVIOUS_SHA` (the last healthy sha), `SAKA_BRANCH`, `SAKA_REPO` and `SAKA_PROJECT`. A `run` action also gets the project secrets listed in `secrets`; the probe and the checks get `verify_secrets`; a `run_host` action never gets a secret, and it follows the host-hook rules: the config file must not be writable by anyone else, it runs in its own cgroup with a timeout, and it is not started again after a restart. A webhook body is `{ id, event: "deploy", environment, project, repo, branch, sha, previous_sha, target_url, tasks, occurred_at }`, signed like a hook webhook, with `X-Saka-Delivery` set to the deployment id so the receiver can drop a repeat.

With `gate = true` (the default) a task whose merge the environment matches stays in the sync stage until the deployment that carries its change succeeds; with `gate = false` the deployment still runs and is recorded but holds nothing. When a deployment fails, each task it holds is blocked with `deploy_unverified` naming the environment, the phase and the code, and the primary action comes from `on_failure`: Revert makes the revert task and Draft a fix asks the architect for a bugfix spec. A fix draft states the facts Saka knows (environment, sha, the last healthy sha, phase, code, the held tasks, your note) and puts the output of the target, which is untrusted text, only in a fenced data block. Other actions are Retry (a new deployment of the branch tip), and Accept without verification (needs a reason, recorded in the audit trail). The blocker closes by itself as soon as a later deployment that carries the change succeeds, whatever started it. A failed deployment that holds no task (`gate = false`, a merge made outside Saka) opens an Inbox item with Redeploy, or Draft a fix when `on_failure = "fix"`, that the next successful deployment closes.

See the evidence in Delivery: the task shows each gate, the Deployments page of the project lists deployments and shows each phase with its commands, durations and output tail, and Settings shows the environments from the config. `saka deploy list <project>`, `saka deploy show <id>`, `saka deploy redeploy <project> <environment> --reason "<why>"` and `saka deploy cancel <id> --reason "<why>"` do the same from the terminal; the API is under `/api/v1/projects/{id}/environments`, `/api/v1/projects/{id}/deployments` and `/api/v1/deployments/{id}`. Hooks `deployed` and `deploy_failed` fire when a deployment ends and take an `environments = [...]` filter; they cannot be gates. `saka_deployments_total{environment,outcome}` and `saka_deploy_checks_total{phase,state}` count what happened. Not in this version: promotion between environments, deploys triggered by tags, and a human approval before a deploy (use `gate = false` and deploy by hand).

## Incidents

An incident is an alert from your monitoring tool that Saka has turned into a work item: one incident per alert fingerprint, linked to the deployment that was running when it fired, with the tasks and merged changes that deployment shipped as suspects. Saka does not page anyone or run an on-call schedule. It takes the signal, shows you what changed, and offers the same three ways out every time: draft a fix, revert a suspect task, or dismiss the signal.

Sources live only in the operator config (`config.toml`); `saka.toml` and `.saka/` cannot define one. Each source belongs to one project, has its own name and secret, and needs the `[hooks]` listener, which also serves the incident endpoint at `POST /hooks/incident/<name>`. Set `[notify] public_url` so Settings shows the full URL to give the tool; without it Settings shows the path only. `saka serve` refuses to start when a secret file is missing, empty, or readable by anyone but you, and `saka doctor` reports the same problems.

```toml
[hooks]
listen = "127.0.0.1:7421"          # behind a reverse proxy; see the security notes below

[[incident.source]]
name = "prod-alerts"               # ^[a-z0-9-]{1,32}$, unique; the last part of the URL
project = "tasks"                  # a project name, or its id
kind = "alertmanager"              # alertmanager | sentry | generic
secret_file = "~/.config/saka/prod-alerts.secret"   # chmod 600
environment_label = "env"          # alertmanager only: the label that names the environment (default "environment")
severity_label = "severity"        # alertmanager only: the label that carries the severity (default "severity")
environments = { prod = "production" }   # optional: a value from the tool -> an environment name in saka.toml
action = "inbox"                   # inbox (default) | draft: draft a fix on its own
max_open = 50                      # 1..=500: open or handled incidents per source
repo = "app"                       # the repo a draft targets; required when the project binds more than one

[[incident.source]]
name = "web-errors"
project = "tasks"
kind = "sentry"
secret_file = "~/.config/saka/web-errors.secret"
action = "draft"
min_severity = "high"              # with action = "draft": critical | high | medium | low | info (default high)
repo = "app"
```

An instance has at most 16 sources. `environment_label`, `severity_label` and `min_severity` are refused on a source where they would do nothing. Create a secret with `umask 077 && openssl rand -hex 32 > ~/.config/saka/prod-alerts.secret`; a trailing newline is ignored.

**Alertmanager.** Point a receiver at the URL and send the secret as a bearer token. `send_resolved: true` matters: the resolved notification is what closes the incident when the alert clears. Saka reads the version 4 payload and makes one signal per alert; the title is the `summary` annotation (or the `alertname` label), the summary is the `description` annotation, and the severity comes from the `severity_label` label (`critical` or `page` is critical, `high` or `error` is high, `warning` or `medium` is medium, `low` is low, anything else is info, no label is medium).

```yaml
receivers:
  - name: saka
    webhook_configs:
      - url: https://saka.example.com/hooks/incident/prod-alerts
        send_resolved: true
        http_config:
          authorization:
            type: Bearer
            credentials_file: /etc/alertmanager/saka-prod-alerts.secret   # the same content as secret_file
```

**Sentry.** In Settings, Developer Settings, create an internal integration. Set its webhook URL to `https://saka.example.com/hooks/incident/web-errors`, turn on the Issue webhook and the Alert Rule Action (the event alert webhook), and save. Sentry shows the integration's client secret: write it to the secret file (`umask 077 && printf '%s' '<client secret>' > ~/.config/saka/web-errors.secret`). Saka checks `Sentry-Hook-Signature`, an HMAC-SHA256 of the body made with that client secret. An issue that is created or unresolved fires, an issue that is resolved clears, and an event alert fires with the environment of the event; every other resource is accepted and ignored. For event alerts, add the integration as an action of an alert rule. The Sentry issue id is the fingerprint and the severity comes from the level (`fatal` is critical, `error` is high, `warning` is medium, the rest is info).

**Generic sender.** For any other tool, sign the request yourself. The body is JSON with exactly these fields, and any other field is refused: `fingerprint` (1 to 200 characters), `status` (`firing` or `resolved`), `severity` (`critical`, `high`, `medium`, `low` or `info`), `title` (one line, up to 200 characters), and optionally `summary` (up to 4 KiB), `environment`, `url`, `started_at` (RFC 3339) and `labels` (up to 32 pairs, keys up to 64 and values up to 256 characters). `X-Saka-Timestamp` is the Unix time in seconds, `X-Saka-Delivery` is an id of your choice (letters, digits, dots, dashes and underscores) that Saka handles once, and `X-Saka-Signature-256` is `sha256=` followed by the hex HMAC-SHA256 of `<timestamp>.<body>` made with the secret.

```sh
body='{"fingerprint":"checkout-5xx","status":"firing","severity":"high","title":"Checkout error rate above 5%","environment":"staging","labels":{"service":"checkout"}}'
ts=$(date +%s)
sig=$(printf '%s.%s' "$ts" "$body" | openssl dgst -sha256 -hmac "$(cat ~/.config/saka/ci-alerts.secret)" -hex | sed 's/^.* //')
curl -fsS -X POST https://saka.example.com/hooks/incident/ci-alerts \
  -H 'Content-Type: application/json' \
  -H "X-Saka-Timestamp: $ts" \
  -H "X-Saka-Delivery: $(uuidgen)" \
  -H "X-Saka-Signature-256: sha256=$sig" \
  --data-binary "$body"
```

The timestamp must be within 300 seconds of the server clock, so keep both ends on NTP. Send the body byte for byte as you signed it. The answer is `202` with `outcome` `accepted`, `duplicate` (a delivery id Saka already handled) or `ignored` (valid, but nothing to act on), and the counts `signals`, `opened`, `updated`, `limited` and `truncated`.

At most one incident is open or being handled per source and fingerprint. A signal that fires again adds to `occurrences`, refreshes `last_seen` and raises the severity when the new one is higher; the severity never goes down while the incident is open. A source that already has `max_open` incidents opens no new one, counts the signal as `limited`, and keeps updating the ones it has. A delivery carries at most 500 alerts; the rest are counted as `truncated`. An incident of severity `info` is recorded but opens no Inbox item, unless the source has `action = "draft"` with `min_severity = "info"`. When the environment named by the signal is known, the incident links to the last succeeded deployment of that environment that finished before the alert started (or before Saka received it, when the alert has no start time). The suspects are the tasks that deployment held and the changes it merged, including pull requests made outside Saka, which are linked but cannot be reverted from here. The link is fixed when the incident opens: it is the evidence of the moment, and a later deployment does not change it.

An open incident shows in Delivery under Incidents (the project's Incidents page, `saka incident list <project>` and `saka incident show <id>`), in the Inbox with the sentence "high incident in staging: Checkout error rate above 5% (3×)", and in your chat when an `inbox_item_opened` hook is set. From there:

- **Draft a fix** (`saka incident draft-fix <id> --note "<text>"`, or automatically for a source with `action = "draft"` and a severity at or above `min_severity`) asks the architect for a bugfix spec. It gets the facts Saka knows (source, severity, environment, first and last seen, occurrences, the deployment and its sha range, the suspects with their pull request links, your note) and the text of the alert only inside a fenced data block. The spec waits for your approval like any other, so nothing is built from an alert alone.
- **Revert** (`saka incident revert <id> <task> --reason "<why>"`) reverts a suspect task as in Reverting a merged task. It is offered for each suspect that Saka ran.
- **Dismiss** (`saka incident dismiss <id> --reason "<why>"`) closes an open incident without a recovery. A firing signal for a dismissed fingerprint is counted but opens nothing until that tool sends a resolved signal.

Only one recovery runs per incident, and the incident is `handling` while it runs. It closes as `resolved` when the fix spec ships (`fixed`), when the revert task is done (`reverted`), or when the tool sends a resolved signal for an incident nobody is handling (`signal_resolved`). If the draft fails or is cancelled, the fix spec is withdrawn, or the revert is cancelled, the incident is open again and its Inbox item says why. An alert that fires again after the incident resolved opens a new incident.

The incident endpoint is public, so treat it as hostile input:

- Run the `[hooks]` listener behind a reverse proxy that terminates TLS and forwards only `/hooks/` to it; the control API stays on loopback and is never served there.
- A request is checked before anything is read or stored: a wrong secret, a bad signature or a stale timestamp gets `401`, a body over 1 MiB gets `413`, and more than 120 deliveries a minute for one source gets `429` with `Retry-After`. A refused request writes nothing.
- The secret file holds the only credential: keep it at mode 0600, one secret per source, and rotate it by replacing the file and the tool's setting together, then restarting `saka serve`.
- The title, summary and labels of an alert are untrusted text. Saka redacts secrets in them, cuts them to size, shows them in the web app as plain text, and gives them to the architect only inside a data block that its instructions never follow.

`saka_incident_deliveries_total{source,outcome}` and `saka_incident_signals_total{source,outcome}` count what each source sent. The API is under `/api/v1/projects/{id}/incident-sources`, `/api/v1/projects/{id}/incidents` and `/api/v1/incidents/{id}`. Not in this version: escalation, on-call schedules, status pages, and merging the same fault reported by two sources.

## Development

| Path | Contents |
|---|---|
| `crates/` | The product crates; the allowed dependencies between them are checked by `cargo xtask deps` |
| `web/` | The web app (Vite, React), embedded in the binary with `--features embed-web` |
| `desktop/` | The desktop shell (Tauri 2), outside the default workspace members; see Desktop app |
| `xtask/` | Repository tasks: `check`, `deps`, `desktop-check`, `desktop-bundle`, `hygiene`, `wasm`, `test-systemd`, `test-claude-memory`, `test-claude-mcp`, `e2e` |
| `spikes/` | Research experiments, excluded from every check |

```sh
make check               # format, lint, tests, saka-wasm bindings, dependency direction, source hygiene, API drift, web checks
cargo xtask wasm         # builds the spec lint for the browser into web/src/wasm/pkg (needed before pnpm dev, test, or build)
make desktop-check       # builds and tests the desktop shell (needs WebKitGTK and GTK headers)
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
- a GitHub App installed on that repository with these repository permissions: Contents read and write, Pull requests read and write, Checks read, Commit statuses read and write (for the `saka/verify` status), Issues read and write (for the issue scenario), Metadata read (no webhook needed). Download a private key and `chmod 600` it;
- a Claude Code account directory that is logged in: point `agent.account_dir` at it and run `saka agent login`.

| Variable | Value |
|---|---|
| `SAKA_E2E_GITHUB_APP_ID` | The App ID |
| `SAKA_E2E_GITHUB_PRIVATE_KEY_FILE` | Path to the private key (mode 0600) |
| `SAKA_E2E_REPO` | `<owner>/<name>` of the test repository |
| `SAKA_E2E_CLAUDE_CONFIG_DIR` | The logged-in Claude Code account directory |
| `SAKA_E2E_MODEL` | Optional; default `haiku` |
| `SAKA_E2E_AUTHOR_TOKEN_FILE` | Optional; a token of an account other than the App that can push to the test repository. Default: `gh auth token` |
| `SAKA_E2E_ISSUE_LIMIT_SECS` | Optional; replaces every wait of the issue scenario with this many seconds, to force a failure and check the cleanup |

The run creates base branch `e2e/<run-id>` from the default branch. It commits a context doc (`docs/e2e-context-<run-id>.md`), a two-step spec that links to it (`xtask/src/e2e/fixture-spec.md.tmpl`), and a second project folder (`docs-review/saka.toml`, preset `review_only`, limited to this run's outside branch). It then starts `saka serve` with a config of its own, registers the project, and approves the spec. The config makes the repo `autonomous` in `[autonomy]`, so steps start on their own and Saka merges each pull request when the review approved its head and CI is green on it. The run never taps merge. A permission request whose command is one of the spec's Done when commands, or `sleep 30` (asked for by the scenario), is allowed; any other request is denied with a hint.

The spec run checks:

- **Runtime.** When S1's session starts, the run reads S1's work packet: the packet carries the pinned context doc, and the doc's marker line sits inside that document. While S1 runs, the run steers its session, interrupts it and steers it again, kills the runner unit so the session resumes on its own, and restarts `saka serve` while the resumed session runs. At the end S1 has a resumed session and a timeline with the steer and the result.
- **Review and automatic merge.** Every task reports `merge_auto`, `saka_merge_requests_total` counts merges by `autonomy` and none by an operator, and each pull request is merged. The reviewer posts on Saka's own pull requests (as comments, since the provider refuses a verdict on one's own pull request).
- **Fix round.** When S2's session starts, the run interrupts it and steers it to write a placeholder instead of quoting S1, so AC-02 is plainly unmet. The reviewer must answer `changes_requested` at that head, a fix round must run and push, a review must follow at the new head with `approved`, and GitHub must show Saka's review at both heads. If the reviewer approves the first head anyway, the run fails with "the fix path was not exercised"; it never passes without the round.
- **Touches.** The run counts the operator's decisions from approve to shipped (the spec approval, and any merge, retry, or start). More than 3 fails the run; it expects 1, because the repo is `autonomous`. The count is printed. The tool permissions the agent asks for are policy, not decisions: they are answered by the run's own rules and listed beside the count, not in it, and so are the scenario's injections (steer, interrupt, kill, restart, `sleep 30`).
- **Finish.** The spec ships within 40 minutes with two merged pull requests and their task branches deleted, and S2 is admitted within 5 minutes of S1's merge.

After the spec, the run proves `review_only` for a pull request from outside. It registers the second project, opens a pull request from a plain branch (`e2e-ext/<run-id>`) as the account behind `gh auth token` (or the token in `SAKA_E2E_AUTHOR_TOKEN_FILE`), because the provider refuses a verdict from the account that opened the pull request. Saka takes it as an `external_change` with no spec, the reviewer posts a native review at the head, and the run checks on GitHub that the review of the App exists at that sha with the state the verdict maps to. Saka must not merge it on its own: once the pull request's checks finish, it is still open, unmerged, and not marked for an automatic merge. Then the run closes it.

The run then proves the issue loop. While preparing the base branch it rewrites `docs/saka.toml` there: `[workflow.issue]` keeps its settings but takes the run's own trigger label `saka-e2e-<run-id>`, and `[workflow.issue.bugfix]` uses only the Go commands, so the result is deterministic and the permission rules allow it. It also plants `cli/internal/e2ebug/clamp.go`, whose `Clamp` returns the lower bound for a value above the range, with a test that passes because it never tries that case. With the author token it creates the run's label and an issue `e2e <run-id>: Clamp returns the lower bound above the range`, labelled `bug` and with the run's label. The run then checks, each with a time limit and a message that names the last state it saw: the intake exists on the `bugfix` lane and reaches `awaiting_approval` with a `specs/bugfix-…-<n>.md` spec, and Saka's `accepted` and `awaiting approval` comments are on the issue; the spec has origin `intake`, kind `bugfix`, and a `## Regression` section, and the run approves it; the task's regression proof is `reproduced` at a commit before the pull request head, with every regression command failed there, a gate passes at the head, and Saka merges the pull request, whose body says `Closes #<n>`, into the base branch; the intake is `done` with a `done` comment naming the base branch; and within 3 minutes the issue is `closed` as `completed` by the App. The base branch is not the default branch, so the `Closes` keyword does nothing there and only Saka's own close can end the issue. Whatever the outcome, the run deletes its label. If the scenario failed and the issue is still open, the run comments `end-to-end run failed, see serve log` and closes it as `not_planned`. The run never touches an issue or label that is not its own.

The run then asks the architect to draft a new spec from a short idea (`saka spec draft … --wait`) and checks that the draft succeeds, its commit is the latest change of that file on the base branch, the spec is registered with origin `architect`, and the architect session used no tool outside its allow-list. Then it starts a conversation (`saka spec session … --wait`), checks that the architect stops and waits, replies asking for a second markdown file, checks that both files are listed as changes with the lint passing, commits, and checks that one commit on the base branch touches both files and registers the spec from the architect. After that the base branch is deleted. When a run fails, the base branch is kept for inspection, and the server log is in `target/e2e/<run-id>/serve.log`.

The run last proves deploy, verify, failure and revert. It starts a small HTTP server on a free loopback port in the `xtask` process: `GET /version` answers the contents of `<run dir>/deploy/sha` (404 until a deployment wrote it) and `GET /health` answers 200. The run's config gets an environment `e2e` with the action `run_host`, `printf %s "$SAKA_SHA" > sha` in `<run dir>/deploy`, `target_url` pointing at that server, `gate = true`, `on_failure = "revert"` and a 60 second timeout; the file is written with mode 0600 as `run_host` requires. The base branch's `docs/saka.toml` gets `[verify.e2e]`: the probe `curl -fsS "$SAKA_DEPLOY_URL/version" | grep -qx "$SAKA_SHA"` every 5 seconds for up to 120, and the checks `curl -fsS "$SAKA_DEPLOY_URL/health"` and `test ! -e cli/E2E_BREAK`. Because the environment gates every task of the project, the first scenarios already run through it. Right after the spec ships, the run checks that each task of the spec has a `succeeded` deployment of `e2e` that lists it, with the `deploy`, `ready` and both `verify` checks passed, that the task became `done` only after that deployment finished, and that `/version` on the target is the sha of the newest deployment. After the conversation, the run creates a `chore` quick spec (`saka spec quick … --approve`) that makes the agent add `cli/E2E_BREAK`. It then checks, in order: the pull request merges; the deployment fails in `verify` with `verify_failed`, `test ! -e cli/E2E_BREAK` failed and the health check passed; the task is blocked with `deploy_unverified` and the primary action `revert`; the run takes that action (`actions/primary`) with a reason; the revert task `<key>-R1` opens a pull request that CI passes and Saka merges; the next deployment `succeeded` at a new sha, with the target serving it; the original task is `cancelled` and the revert task `done`. The log prints each deployment id and sha, the failing check, the revert pull request number and the final state of both tasks. The step that redoes the task after a revert is skipped until redo ships (`skip redo (not shipped)`). The permission rules also allow the Done when command of the quick spec, `test -f cli/E2E_BREAK`. The server stops and the deploy directory is removed at the end, whatever the outcome.

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
