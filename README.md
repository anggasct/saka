# Saka

A self-hosted delivery loop for coding agents: an approved spec becomes one task per step, each task runs an agent session in its own worktree, and Saka opens, gates, and merges the pull request before starting the steps that wait for it.

## Running

```sh
cargo build --release -p saka --features embed-web   # after `corepack pnpm build` in web/
saka serve                                            # API, workers, and the web app on 127.0.0.1:7420
saka project add https://github.com/<owner>/<repo> --path docs
saka spec template > docs/specs/<spec>.md            # a spec that passes lint (--locale id for Indonesian)
saka spec approve <project> docs/specs/<spec>.md       # again after an edit: repins steps that have not started
saka spec withdraw <project> docs/specs/<spec>.md --reason "<why>"
saka task list
saka task steer <task> "<message>"                     # also: interrupt, stop --reason, timeline
saka permission answer <request> allow --remember      # allow the same request in this project from now on
```

A spec's context is pinned with it. Saka reads two kinds of markdown doc at the same commit as the spec and stores them with each revision:

- docs the spec links to with ordinary markdown links;
- docs in the project folder whose frontmatter says `context: always`.

A changed doc makes a new revision. Approving it again repins the steps that have not started. A link to a missing doc, or context over 32 docs or 256 KiB, is a lint error that blocks approve. Each task's first turn puts these documents first, the spec as document 1 and its context after it. The step, its acceptance criteria, its Done when commands, and the environment's ports, toolchains, and session secret names follow. `GET /api/v1/work-items/<id>/work-packet` returns that turn exactly as the agent received it.

Configuration lives in `$XDG_CONFIG_HOME/saka/config.toml` (or the file named by `SAKA_CONFIG`); [`docs/config.example.toml`](docs/config.example.toml) lists every field with its default. Saka needs a GitHub App installed on the repository (see [End-to-end run](#end-to-end-run) for the permissions) and a Claude Code account directory that is already logged in.

## Development

| Path | Contents |
|---|---|
| `crates/` | The product crates; the allowed dependencies between them are checked by `cargo xtask deps` |
| `web/` | The web app (Vite, React), embedded in the binary with `--features embed-web` |
| `xtask/` | Repository tasks: `check`, `deps`, `hygiene`, `test-systemd`, `test-claude-memory`, `e2e` |
| `spikes/` | Research experiments, excluded from every check |

```sh
make check               # format, lint, tests, dependency direction, source hygiene, API drift, web checks
make test-systemd        # the session supervisor in a real user systemd unit
make test-claude-memory  # the installed claude CLI loads instructions only from the session worktree
make e2e                 # both of the above, then one real delivery loop (needs credentials, see below)
```

The Rust toolchain is pinned in `rust-toolchain.toml`; Node and pnpm in `web/package.json` (run pnpm as `corepack pnpm` from `web/`). On a shared machine `make` runs each target through `scripts/limited`; set `CI=true` to skip that.

## End-to-end run

`make e2e` drives one real loop: Claude Code writes the code, GitHub runs CI, and Saka merges. It is not part of CI and needs:

- a user systemd instance (`systemctl --user is-system-running` says `running` or `degraded`), `git`, and the `claude` CLI;
- a test repository you own, with a `docs/saka.toml` naming the repository itself, a Go module in `cli/`, and a CI workflow that runs on `pull_request` for any base branch;
- a GitHub App installed on that repository with these repository permissions: Contents read and write, Pull requests read and write, Checks read, Commit statuses read, Metadata read (no webhook needed). Download a private key and `chmod 600` it;
- a Claude Code account directory that is already logged in: `CLAUDE_CONFIG_DIR=<dir> claude`, then `/login`.

| Variable | Value |
|---|---|
| `SAKA_E2E_GITHUB_APP_ID` | The App ID |
| `SAKA_E2E_GITHUB_PRIVATE_KEY_FILE` | Path to the private key (mode 0600) |
| `SAKA_E2E_REPO` | `<owner>/<name>` of the test repository |
| `SAKA_E2E_CLAUDE_CONFIG_DIR` | The logged-in Claude Code account directory |
| `SAKA_E2E_MODEL` | Optional; default `haiku` |

The run creates base branch `e2e/<run-id>` from the default branch. It commits a context doc (`docs/e2e-context-<run-id>.md`) and a two-step spec that links to it (`xtask/src/e2e/fixture-spec.md.tmpl`). It then starts `saka serve` with a config of its own, registers the project, approves the spec, and merges each step through the CLI once CI passes. When S1's session starts, the run reads S1's work packet and checks two things: the packet carries the pinned context doc, and the doc's marker line sits inside that document. While S1 runs, the run steers its session, interrupts it and steers it again, kills the runner unit so the session resumes on its own, and restarts `saka serve` while the resumed session runs; at the end it checks that S1 has a resumed session and a timeline with the steer and the result. A permission request whose command is one of the spec's Done when commands, or `sleep 30` (asked for by the scenario), is allowed; any other request is denied with a hint. The run must finish within 30 minutes with the spec shipped, two merged pull requests, and their task branches deleted. After that the base branch is deleted. When a run fails, the base branch is kept for inspection, and the server log is in `target/e2e/<run-id>/serve.log`.

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
