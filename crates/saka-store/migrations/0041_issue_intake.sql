CREATE TABLE issue_intake (
  id             TEXT    PRIMARY KEY,
  project_id     TEXT    NOT NULL REFERENCES project(id),
  repo_id        TEXT    NOT NULL REFERENCES repo(id),
  number         INTEGER NOT NULL CHECK (number >= 1),
  url            TEXT    NOT NULL CHECK (length(url) BETWEEN 1 AND 2048),
  title          TEXT    NOT NULL CHECK (length(title) BETWEEN 1 AND 120),
  author         TEXT    NOT NULL CHECK (length(author) BETWEEN 1 AND 255),
  state          TEXT    NOT NULL CHECK (state IN ('accepted','drafting','awaiting_approval','approved','in_progress','done','dismissed','closed','failed')),
  lane           TEXT    NOT NULL CHECK (lane IN ('bugfix','draft')),
  kind           TEXT    NOT NULL CHECK (kind IN ('feature','bugfix','refactor','chore','security_patch')),
  cause          TEXT    NOT NULL CHECK (cause IN ('label','comment','author','operator')),
  actor          TEXT    NOT NULL,
  spec_id        TEXT    REFERENCES spec(id),
  draft_id       TEXT    REFERENCES spec_draft(id),
  failure        TEXT    CHECK (failure IS NULL OR json_valid(failure)),
  retries        INTEGER NOT NULL DEFAULT 0 CHECK (retries BETWEEN 0 AND 3),
  created_at     INTEGER NOT NULL,
  updated_at     INTEGER NOT NULL,
  CHECK ((state = 'failed') = (failure IS NOT NULL))
) STRICT;

CREATE UNIQUE INDEX issue_intake_one_live ON issue_intake (repo_id, number)
  WHERE state NOT IN ('dismissed','closed','done');
CREATE INDEX issue_intake_project_state ON issue_intake (project_id, state);
CREATE INDEX issue_intake_spec ON issue_intake (spec_id) WHERE spec_id IS NOT NULL;
CREATE INDEX issue_intake_draft ON issue_intake (draft_id) WHERE draft_id IS NOT NULL;

CREATE TABLE issue_signal_cursor (
  repo_id     TEXT    NOT NULL REFERENCES repo(id),
  number      INTEGER NOT NULL,
  updated_at  INTEGER NOT NULL,
  through     INTEGER NOT NULL,
  seen_ids    TEXT    NOT NULL CHECK (json_valid(seen_ids)),
  PRIMARY KEY (repo_id, number)
) STRICT;

CREATE TABLE issue_scan_cursor (
  project_id  TEXT    NOT NULL REFERENCES project(id),
  repo_id     TEXT    NOT NULL REFERENCES repo(id),
  through     INTEGER NOT NULL,
  PRIMARY KEY (project_id, repo_id)
) STRICT;

ALTER TABLE spec_draft ADD COLUMN kind TEXT NOT NULL DEFAULT 'feature'
  CHECK (kind IN ('feature','bugfix','refactor','chore','security_patch'));

CREATE TABLE inbox_item_v40 (
  id            TEXT PRIMARY KEY,
  project_id    TEXT NOT NULL REFERENCES project(id),
  kind          TEXT NOT NULL CHECK (kind IN ('decision','approval','permission','blocker','spec_unclear','intake')),
  subject       TEXT NOT NULL CHECK (subject IN ('spec','step_start','merge','permission','blocker','decision','intake')),
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

INSERT INTO inbox_item_v40 (id, project_id, kind, subject, source_id, work_item_id, spec_id, state, params, action, opened_at, answered_at, closed_at)
SELECT id, project_id, kind, subject, source_id, work_item_id, spec_id, state, params, action, opened_at, answered_at, closed_at FROM inbox_item;

DROP TABLE inbox_item;
ALTER TABLE inbox_item_v40 RENAME TO inbox_item;

CREATE UNIQUE INDEX inbox_item_live ON inbox_item(subject, source_id) WHERE state IN ('open','answered');
CREATE INDEX inbox_item_list ON inbox_item(state, opened_at DESC, id DESC);
CREATE INDEX inbox_item_project ON inbox_item(project_id, state);
CREATE INDEX inbox_item_work_item ON inbox_item(work_item_id) WHERE work_item_id IS NOT NULL;
CREATE INDEX inbox_item_spec ON inbox_item(spec_id) WHERE spec_id IS NOT NULL;
