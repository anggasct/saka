---
saka: spec
owner: example
---
# Mark tasks as done

Free text before the contract is fine.

## Acceptance criteria

- **AC-01**: `tasks done <id>` marks the task done and `tasks list` shows `[x]`.
- **AC-02**: the web page shows done tasks with a check mark.
- Notes without an id are ignored.

## Steps

| Step | Work | AC | Done when |
|------|------|----|-----------|
| S1 | Add a `done` command to the CLI | AC-01 | `cd cli && go test ./...` |
| S2 | Show done tasks in the web page | AC-01, AC-02 | `cd web && npm test` `cd cli && go vet ./...` |

## Notes

| Other | Table |
|---|---|
| is | ignored |
