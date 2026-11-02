---
saka: spec
---
# Decisions fixture

## Acceptance criteria

- **AC-01**: one.

## Done when

- `true`

## Decisions

### Q1: Where are uploaded files stored?

- [x] **Local disk** (recommended) — one node only.
- [ ] **Object storage** — scales out.

### Q2: How are thumbnails made?

- [ ] On upload (recommended)
- [X] On first read
