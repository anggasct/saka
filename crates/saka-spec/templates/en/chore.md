---
saka: spec
kind: chore
# depends_on: [other-spec]   specs that must ship before this one starts
# priority: 0                -1000..1000; higher starts first
---
# Short title of the chore

What needs doing and why, for example a dependency bump, a config change, or a cleanup.

## Acceptance criteria

- **AC-01**: one observable result that a command can check.

## Done when

- `make test`

## Notes

- A chore changes nothing users can see. Use the `feature` kind when behaviour changes and `bugfix` when you are fixing a bug.
- Every command in the `Done when` list must pass before the step is done.
