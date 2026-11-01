---
# a full-line comment
saka: "spec"   # the marker may be quoted
owner: someone
review:
  people:
    - a: [b, c]
  note: "free # text"
priority: '10'
depends_on:
  - auth-model
  - "billing"
  - auth-model   # duplicates are ignored
labels: [x, y]
---
# Full frontmatter

## Acceptance criteria

- **AC-01**: one.

## Steps

| Step | Work | AC | Done when |
|------|------|----|-----------|
| S1 | x | AC-01 | `true` |
