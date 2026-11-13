ALTER TABLE incident ADD COLUMN released TEXT
  CHECK (released IS NULL OR released IN ('draft_failed','spec_withdrawn','revert_cancelled'));

ALTER TABLE spec_draft ADD COLUMN incident_id TEXT REFERENCES incident(id);
CREATE INDEX spec_draft_incident_idx ON spec_draft (incident_id) WHERE incident_id IS NOT NULL;

CREATE TABLE incident_delivery (
  id           TEXT    PRIMARY KEY,
  source       TEXT    NOT NULL CHECK (length(source) BETWEEN 1 AND 32),
  delivery_key TEXT    NOT NULL CHECK (length(delivery_key) BETWEEN 1 AND 128),
  received_at  INTEGER NOT NULL,
  outcome      TEXT    NOT NULL CHECK (outcome IN ('accepted','ignored')),
  signals      INTEGER NOT NULL CHECK (signals >= 0),
  UNIQUE (source, delivery_key)
) STRICT;

CREATE TABLE inbox_item_v47 (
  id            TEXT PRIMARY KEY,
  project_id    TEXT NOT NULL REFERENCES project(id),
  kind          TEXT NOT NULL CHECK (kind IN ('decision','approval','permission','blocker','spec_unclear','intake','deployment','incident')),
  subject       TEXT NOT NULL CHECK (subject IN ('spec','step_start','merge','permission','blocker','decision','intake','deployment','incident')),
  source_id     TEXT NOT NULL,
  work_item_id  TEXT REFERENCES work_item(id),
  spec_id       TEXT REFERENCES spec(id),
  state         TEXT NOT NULL CHECK (state IN ('open','answered','resolved','dismissed')),
  params        TEXT NOT NULL CHECK (json_valid(params)),
  action        TEXT NOT NULL CHECK (json_valid(action)),
  opened_at     INTEGER NOT NULL,
  answered_at   INTEGER,
  closed_at     INTEGER
) STRICT;

INSERT INTO inbox_item_v47 (id, project_id, kind, subject, source_id, work_item_id, spec_id, state, params, action, opened_at, answered_at, closed_at)
SELECT id, project_id, kind, subject, source_id, work_item_id, spec_id, state, params, action, opened_at, answered_at, closed_at FROM inbox_item;

DROP TABLE inbox_item;
ALTER TABLE inbox_item_v47 RENAME TO inbox_item;

CREATE UNIQUE INDEX inbox_item_live ON inbox_item(subject, source_id) WHERE state IN ('open','answered');
CREATE INDEX inbox_item_list ON inbox_item(state, opened_at DESC, id DESC);
CREATE INDEX inbox_item_project ON inbox_item(project_id, state);
CREATE INDEX inbox_item_work_item ON inbox_item(work_item_id) WHERE work_item_id IS NOT NULL;
CREATE INDEX inbox_item_spec ON inbox_item(spec_id) WHERE spec_id IS NOT NULL;
