CREATE TABLE spec_revision_v2 (
  id             TEXT    PRIMARY KEY,
  spec_id        TEXT    NOT NULL REFERENCES spec(id),
  project_id     TEXT    NOT NULL REFERENCES project(id),
  source_commit  TEXT    NOT NULL CHECK (length(source_commit) = 40),
  content_hash   TEXT    NOT NULL,
  content        BLOB    NOT NULL CHECK (length(content) <= 262144),
  lint_ok        INTEGER NOT NULL CHECK (lint_ok IN (0,1)),
  lint_issues    TEXT    NOT NULL CHECK (json_valid(lint_issues)),
  model          TEXT    NOT NULL CHECK (json_valid(model)),
  parser_version INTEGER NOT NULL CHECK (parser_version >= 1),
  created_at     INTEGER NOT NULL,
  UNIQUE (spec_id, content_hash, parser_version)
) STRICT;

INSERT INTO spec_revision_v2 (id, spec_id, project_id, source_commit, content_hash, content, lint_ok,
  lint_issues, model, parser_version, created_at)
SELECT id, spec_id, project_id, source_commit, content_hash, content, lint_ok,
  lint_issues, model, parser_version, created_at
FROM spec_revision;

DROP TABLE spec_revision;
ALTER TABLE spec_revision_v2 RENAME TO spec_revision;
CREATE INDEX spec_revision_spec_idx ON spec_revision (spec_id, created_at);

CREATE TABLE spec_dependency (
  spec_id            TEXT    NOT NULL REFERENCES spec(id),
  depends_on_spec_id TEXT    NOT NULL REFERENCES spec(id),
  depends_on_slug    TEXT    NOT NULL,
  revision_id        TEXT    NOT NULL REFERENCES spec_revision(id),
  project_id         TEXT    NOT NULL REFERENCES project(id),
  created_at         INTEGER NOT NULL,
  PRIMARY KEY (spec_id, depends_on_spec_id),
  CHECK (spec_id <> depends_on_spec_id)
) STRICT, WITHOUT ROWID;
CREATE INDEX spec_dependency_target_idx ON spec_dependency (depends_on_spec_id);

CREATE TABLE work_item_dependency (
  work_item_id            TEXT    NOT NULL REFERENCES work_item(id),
  depends_on_work_item_id TEXT    NOT NULL REFERENCES work_item(id),
  project_id              TEXT    NOT NULL REFERENCES project(id),
  created_at              INTEGER NOT NULL,
  PRIMARY KEY (work_item_id, depends_on_work_item_id),
  CHECK (work_item_id <> depends_on_work_item_id)
) STRICT, WITHOUT ROWID;
CREATE INDEX work_item_dependency_target_idx ON work_item_dependency (depends_on_work_item_id);

INSERT INTO work_item_dependency (work_item_id, depends_on_work_item_id, project_id, created_at)
SELECT w.id, p.id, w.project_id, w.created_at
FROM work_item w
JOIN work_item p
  ON p.spec_id = w.spec_id
 AND p.spec_revision_id = w.spec_revision_id
 AND p.step_index = w.step_index - 1
WHERE w.origin = 'spec_step';
