# Saka

A self-hosted delivery loop for coding agents.

You write and approve a spec. Saka runs an agent on each step, opens the pull
request, has a second agent review it, fixes what the review finds, and merges
once the checks pass. It stops only when a person has to decide something.

```
spec → approve → agent → pull request → CI → review ⇄ fix → merge → next step
```

> **Early and in active development.** It runs end to end on GitHub with Claude
> Code. There is no release yet, so you build it from source, and things will change.

## How it works

- A spec is a markdown file in your repo, with acceptance criteria and a table of steps.
- Each step becomes one task, one agent session in its own worktree, and one pull request.
- A step is done when its merge is proven, not when an agent says so.
- You choose per repo how much runs on its own: `strict`, `standard`, or `autonomous`.
- Everything runs on your machine, and you watch it from a web app or the `saka` CLI.

## Getting started

See the [guide](docs/guide.md) to build Saka, connect a repo, and run your first spec.

## License

[Apache-2.0](LICENSE)
