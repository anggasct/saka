CREATE TABLE inbox_item (
  id            TEXT PRIMARY KEY,
  project_id    TEXT NOT NULL REFERENCES project(id),
  kind          TEXT NOT NULL CHECK (kind IN ('decision','approval','permission','blocker','spec_unclear')),
  subject       TEXT NOT NULL CHECK (subject IN ('spec','step_start','merge','permission','blocker','decision')),
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

CREATE UNIQUE INDEX inbox_item_live ON inbox_item(subject, source_id) WHERE state IN ('open','answered');
CREATE INDEX inbox_item_list ON inbox_item(state, opened_at DESC, id DESC);
CREATE INDEX inbox_item_project ON inbox_item(project_id, state);
CREATE INDEX inbox_item_work_item ON inbox_item(work_item_id) WHERE work_item_id IS NOT NULL;
CREATE INDEX inbox_item_spec ON inbox_item(spec_id) WHERE spec_id IS NOT NULL;
