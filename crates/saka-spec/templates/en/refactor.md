---
saka: spec
kind: refactor
# depends_on: [other-spec]   specs that must ship before this one starts
# priority: 0                -1000..1000; higher starts first
---
# Short title of the refactor

What changes in the structure of the code and why. Behaviour stays the same.
Name the files or modules involved if you know them.

## Acceptance criteria

- **AC-01**: behaviour is unchanged: the existing tests pass without edits to their expectations.

## Done when

- `make test`

## Notes

- A refactor changes structure, not behaviour. If a test has to change, say why in the description.
- Every command in the `Done when` list must pass before the step is done.
