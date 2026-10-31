# Saka (spikes)

Throwaway experiments from the research phase. Each folder answers one question; the measured results are recorded in the research notes, not here.

| Folder | Question | Run |
|---|---|---|
| `spikes/s1-agent` | Can our own runtime drive Claude Code and Codex headless: stream, steer, interrupt, kill and resume, approvals? | `cargo run -p s1-agent -- claude 20` (expects a bare remote at `.dev/s1/origin.git`) |
| `spikes/s2-editor` | Is a CodeMirror 6 live-preview editor lossless on a real vault, and does it resolve links like Obsidian? | `cargo run -p s2-editor -- <vault-copy> spikes/s2-editor/web/dist`, then `pnpm exec node tests/drive.mjs roundtrip` in `web/` |
| `spikes/s4-spec` | How many existing specs fit the task model as written, and which markdown parser to use? | `cargo run --release -p s4-spec -- specs <vault> <project>...` |
| `spikes/s5-toolchain` | Can every repository run with its own Go, Node, and pnpm versions without a version manager? | `cargo run -p s5-toolchain -- spikes/s5-toolchain/plan.json round1` (edit the example repos first) |

Toolchains are pinned in the repository: Rust in `rust-toolchain.toml`, Node and pnpm in `spikes/s2-editor/web/package.json` (`devEngines.runtime`, `packageManager`). `scripts/limited` runs a command in its own cgroup, so builds and agent sessions cannot starve a shared machine. Local data goes to `.dev/`, which is ignored.
