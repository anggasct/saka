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

Uploads are user avatars, at most 2 MB each.

- [x] **Local disk** (recommended) — one node only;
  nothing to configure.
- [ ] **S3-compatible storage** — scales out; needs credentials.
- [ ] **Both, chosen per project** — flexible.

#### Notes

- not an option list

### Q2: How are thumbnails made?

1. [ ] On upload (RECOMMENDED)
2. [ ] On first read

## Notes

### Q3: not a decision outside the section
