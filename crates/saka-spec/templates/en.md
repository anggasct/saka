---
saka: spec
# depends_on: [other-spec]   specs that must ship before this one starts
# priority: 0                -1000..1000; higher starts first
---
# Short title of the change

Describe the problem, who it is for, and what changes when this ships.
Name the files or modules involved if you know them.

## Acceptance criteria

- **AC-01**: one observable result that a command can check.
- **AC-02**: another result, for example an API reply or a test that passes.

## Steps

| Step | Work | AC | Done when |
|------|------|----|-----------|
| S1 | First piece of work | AC-01 | `make test` |
| S2 | Second piece of work, built on S1 | AC-02 | `make test` `make lint` |

## Notes

- Steps run in order. To run steps in parallel, add an `After` column and list the steps each one waits for, such as `S1` or `S1, S2`. An empty cell means the step waits for nothing.
- For a small change, replace the Steps section with a `Done when` section: a list with one command in backticks per item.
- Every command in the `Done when` column must pass before a step is done.
