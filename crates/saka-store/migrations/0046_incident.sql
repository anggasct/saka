CREATE TABLE incident (
  id             TEXT    PRIMARY KEY,
  project_id     TEXT    NOT NULL REFERENCES project(id),
  source         TEXT    NOT NULL CHECK (length(source) BETWEEN 1 AND 32),
  fingerprint    TEXT    NOT NULL CHECK (length(fingerprint) BETWEEN 1 AND 200),
  state          TEXT    NOT NULL CHECK (state IN ('open','handling','resolved','dismissed')),
  resolution     TEXT    CHECK (resolution IS NULL OR resolution IN ('fixed','reverted','signal_resolved')),
  signal_status  TEXT    NOT NULL CHECK (signal_status IN ('firing','resolved')),
  severity       TEXT    NOT NULL CHECK (severity IN ('critical','high','medium','low','info')),
  title          TEXT    NOT NULL CHECK (length(title) BETWEEN 1 AND 800),
  summary        TEXT    NOT NULL CHECK (length(CAST(summary AS BLOB)) <= 4096),
  environment    TEXT,
  url            TEXT,
  labels         TEXT    NOT NULL CHECK (json_valid(labels)),
  occurrences    INTEGER NOT NULL CHECK (occurrences >= 1),
  started_at     INTEGER,
  first_seen     INTEGER NOT NULL,
  last_seen      INTEGER NOT NULL,
  deployment_id  TEXT    REFERENCES deployment(id),
  draft_id       TEXT    REFERENCES spec_draft(id),
  revert_id      TEXT    REFERENCES work_item(id),
  dismiss_reason TEXT,
  actor          TEXT,
  resolved_at    INTEGER,
  created_at     INTEGER NOT NULL,
  updated_at     INTEGER NOT NULL,
  CHECK ((state = 'resolved') = (resolution IS NOT NULL)),
  CHECK ((state = 'resolved') = (resolved_at IS NOT NULL)),
  CHECK ((state = 'dismissed') = (dismiss_reason IS NOT NULL))
) STRICT;

CREATE UNIQUE INDEX incident_one_live ON incident (source, fingerprint)
  WHERE state IN ('open','handling');
CREATE INDEX incident_latest ON incident (source, fingerprint, created_at DESC, id DESC);
CREATE INDEX incident_project_list ON incident (project_id, state, last_seen DESC, id DESC);
CREATE INDEX incident_deployment ON incident (deployment_id) WHERE deployment_id IS NOT NULL;
