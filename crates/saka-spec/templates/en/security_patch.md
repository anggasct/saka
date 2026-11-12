---
saka: spec
kind: security_patch
# depends_on: [other-spec]   specs that must ship before this one starts
# priority: 0                -1000..1000; higher starts first
---
# Short title of the security fix

The weakness, where it lives, and who is affected. Keep exploit details out of the description when the repository is public.
Name the files or modules involved if you know them.

## Acceptance criteria

- **AC-01**: the weakness is closed, shown by a check that fails without the patch.

## Done when

- `make test`

## Notes

- Saka always reviews a security patch and asks the operator to approve each step and the merge, whatever the project workflow says.
- Every command in the `Done when` list must pass before the step is done.
