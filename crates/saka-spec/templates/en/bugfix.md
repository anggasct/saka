---
saka: spec
kind: bugfix
# depends_on: [other-spec]   specs that must ship before this one starts
# priority: 0                -1000..1000; higher starts first
---
# Short title of the bug fix

What goes wrong, the steps that show it, and what should happen instead.
Name the files or modules involved if you know them.

## Acceptance criteria

- **AC-01**: the bug no longer happens, shown by a test that failed before the fix.

## Regression

- `make test-regression`

## Done when

- `make test`

## Notes

- Test first: the agent writes a test that reproduces the bug, changes only test files, and commits it before touching the fix. Saka runs every `## Regression` command on that commit and requires at least one to fail. Only then does the agent fix the bug.
- Each `## Regression` item holds exactly one command in backticks, at most 10. Point it at the new test, not at the whole suite.
- Every command in `## Regression` and `## Done when` must pass on the final change.
- Test files are recognised by path (`tests/`, `*_test.*`, `*.spec.*` and similar). A repository with another layout lists its patterns as `test_paths` for this kind in `saka.toml`.
- The test-first rule applies to the first step only, so keep a bug fix to one step.
