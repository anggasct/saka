pub const TWO_STEP_PATH: &str = "docs/specs/tasks-done.md";

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

pub const AFTER_GRAPH_PATH: &str = "docs/specs/parallel.md";

pub const AFTER_GRAPH_SPEC: &str = "---
saka: spec
---
# Parallel steps

## Acceptance criteria

- **AC-01**: the base works.
- **AC-02**: both sides work.

## Steps

| Step | Work | AC | After | Done when |
|------|------|----|-------|-----------|
| S1 | Base | AC-01 | | `make test` |
| S2 | Left side | AC-02 | S1 | `make test` |
| S3 | Right side | AC-02 | S1 | `make test` |
| S4 | Join both sides | AC-01, AC-02 | S2, S3 | `make test` |
";

pub fn one_step_spec(title: &str, frontmatter: &str) -> String {
    format!(
        "---
saka: spec
{frontmatter}---
# {title}

## Acceptance criteria

- **AC-01**: {title} works.

## Done when

- `make test`
"
    )
}

pub const APPROVAL_PATH: &str = "docs/specs/approval.md";

pub fn approval_spec(frontmatter: &str, steps: &[(&str, &str)]) -> String {
    let rows: String = steps
        .iter()
        .enumerate()
        .map(|(i, (work, ac))| format!("| S{} | {work} | {ac} | `make test` |\n", i + 1))
        .collect();
    format!(
        "---
saka: spec
{frontmatter}---
# Approval

## Acceptance criteria

- **AC-01**: the first part works.
- **AC-02**: the second part works.

## Steps

| Step | Work | AC | Done when |
|------|------|----|-----------|
{rows}"
    )
}
