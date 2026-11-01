---
saka: spec
---
# Parallel steps

## Acceptance criteria

- **AC-01**: one.
- **AC-02**: two.

## Steps

| Step | Work | AC | After | Done when |
|------|------|----|-------|-----------|
| S1 | Base | AC-01 | | `true` |
| S2 | Left | AC-01 | S1 | `true` |
| S3 | Right | AC-02 | S1 | `true` |
| S4 | Join | AC-01, AC-02 | S2, S3, S2 | `true` |
