CREATE TABLE spec_revision_v3 (
  id             TEXT    PRIMARY KEY,
  spec_id        TEXT    NOT NULL REFERENCES spec(id),
  project_id     TEXT    NOT NULL REFERENCES project(id),
  source_commit  TEXT    NOT NULL CHECK (length(source_commit) = 40),
  content_hash   TEXT    NOT NULL,
  context_hash   TEXT    NOT NULL,
  content        BLOB    NOT NULL CHECK (length(content) <= 262144),
  lint_ok        INTEGER NOT NULL CHECK (lint_ok IN (0,1)),
  lint_issues    TEXT    NOT NULL CHECK (json_valid(lint_issues)),
  model          TEXT    NOT NULL CHECK (json_valid(model)),
  parser_version INTEGER NOT NULL CHECK (parser_version >= 1),
  created_at     INTEGER NOT NULL,
  UNIQUE (spec_id, content_hash, context_hash, parser_version)
) STRICT;

INSERT INTO spec_revision_v3 (id, spec_id, project_id, source_commit, content_hash, context_hash, content,
  lint_ok, lint_issues, model, parser_version, created_at)
SELECT id, spec_id, project_id, source_commit, content_hash,
  'sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855', content,
  lint_ok, lint_issues, model, parser_version, created_at
FROM spec_revision;

DROP TABLE spec_revision;
ALTER TABLE spec_revision_v3 RENAME TO spec_revision;
CREATE INDEX spec_revision_spec_idx ON spec_revision (spec_id, created_at);

CREATE TABLE context_blob (
  project_id   TEXT    NOT NULL REFERENCES project(id),
  content_hash TEXT    NOT NULL CHECK (length(content_hash) = 71),
  content      BLOB    NOT NULL CHECK (length(content) <= 262144),
  created_at   INTEGER NOT NULL,
  PRIMARY KEY (project_id, content_hash)
) STRICT, WITHOUT ROWID;

CREATE TABLE spec_revision_context (
  revision_id  TEXT    NOT NULL REFERENCES spec_revision(id),
  ordinal      INTEGER NOT NULL CHECK (ordinal BETWEEN 1 AND 32),
  project_id   TEXT    NOT NULL REFERENCES project(id),
  path         TEXT    NOT NULL,
  origin       TEXT    NOT NULL CHECK (origin IN ('link','always')),
  line         INTEGER NOT NULL CHECK (line >= 1),
  content_hash TEXT    NOT NULL,
  bytes        INTEGER NOT NULL CHECK (bytes BETWEEN 0 AND 262144),
  PRIMARY KEY (revision_id, ordinal),
  UNIQUE (revision_id, path),
  FOREIGN KEY (project_id, content_hash) REFERENCES context_blob(project_id, content_hash)
) STRICT, WITHOUT ROWID;
