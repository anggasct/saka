//! Shared fixtures.

/// Path of [`TWO_STEP_SPEC`] in the project repo.
pub const TWO_STEP_PATH: &str = "docs/specs/tasks-done.md";

/// A valid spec with two steps.
pub const TWO_STEP_SPEC: &str = "---
saka: spec
---
# Mark tasks as done

## Acceptance criteria

- **AC-01**: `tasks done <id>` marks a task as done.
- **AC-02**: `tasks list` shows done tasks with a check mark.

## Steps

| Step | Work | AC | Done when |
|------|------|----|-----------|
| S1 | Add the done command | AC-01 | `cd cli && go test ./...` |
| S2 | Show done tasks in the list | AC-02 | `cd cli && go test ./...` `npm --prefix web test` |
";
