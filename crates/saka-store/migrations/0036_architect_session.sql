CREATE TABLE spec_draft_v36 (
  id                 TEXT    PRIMARY KEY,
  project_id         TEXT    NOT NULL REFERENCES project(id),
  path               TEXT    NOT NULL CHECK (length(path) BETWEEN 1 AND 1024),
  slug               TEXT    NOT NULL,
  idea               TEXT    NOT NULL CHECK (length(idea) BETWEEN 1 AND 8000),
  locale             TEXT    NOT NULL CHECK (locale IN ('en','id')),
  mode               TEXT    NOT NULL DEFAULT 'draft' CHECK (mode IN ('draft','session')),
  context_paths      TEXT    NOT NULL CHECK (json_valid(context_paths) AND json_type(context_paths) = 'array'
                                             AND json_array_length(context_paths) <= 16),
  state              TEXT    NOT NULL CHECK (state IN
                     ('queued','preparing','drafting','awaiting_operator','committing','succeeded','failed',
                      'cancelled')),
  attempt            INTEGER NOT NULL DEFAULT 0 CHECK (attempt >= 0),
  current_attempt_id TEXT,
  retries            INTEGER NOT NULL DEFAULT 0 CHECK (retries BETWEEN 0 AND 3),
  queued_at          INTEGER NOT NULL,
  awaiting_since     INTEGER,
  failure            TEXT    CHECK (failure IS NULL OR json_valid(failure)),
  spec_id            TEXT    REFERENCES spec(id),
  commit_sha         TEXT    CHECK (commit_sha IS NULL OR length(commit_sha) = 40),
  version            INTEGER NOT NULL CHECK (version >= 1),
  created_by         TEXT    NOT NULL,
  created_at         INTEGER NOT NULL,
  updated_at         INTEGER NOT NULL,
  finished_at        INTEGER,
  CHECK ((state = 'failed') = (failure IS NOT NULL)),
  CHECK (state <> 'succeeded' OR (spec_id IS NOT NULL AND commit_sha IS NOT NULL)),
  CHECK ((state IN ('succeeded','failed','cancelled')) = (finished_at IS NOT NULL)),
  CHECK ((attempt = 0) = (current_attempt_id IS NULL)),
  CHECK (state <> 'queued' OR attempt = retries),
  CHECK (state IN ('queued','cancelled') OR attempt = retries + 1),
  CHECK ((state = 'awaiting_operator') = (awaiting_since IS NOT NULL)),
  CHECK (state <> 'awaiting_operator' OR mode = 'session')
) STRICT;

INSERT INTO spec_draft_v36 (id, project_id, path, slug, idea, locale, context_paths, state, attempt,
  current_attempt_id, retries, queued_at, failure, spec_id, commit_sha, version, created_by, created_at,
  updated_at, finished_at)
SELECT id, project_id, path, slug, idea, locale, context_paths, state, attempt,
  current_attempt_id, retries, queued_at, failure, spec_id, commit_sha, version, created_by, created_at,
  updated_at, finished_at
FROM spec_draft;

DROP TABLE spec_draft;
ALTER TABLE spec_draft_v36 RENAME TO spec_draft;
CREATE INDEX spec_draft_project_idx ON spec_draft (project_id, created_at);
CREATE INDEX spec_draft_state_idx ON spec_draft (state, queued_at);
CREATE UNIQUE INDEX spec_draft_one_live_path ON spec_draft (project_id, path)
  WHERE state IN ('queued','preparing','drafting','awaiting_operator','committing','failed');

CREATE TABLE spec_draft_lint_round_v36 (
  attempt_id     TEXT    NOT NULL REFERENCES spec_draft_attempt(id),
  round          INTEGER NOT NULL CHECK (round >= 1),
  spec_draft_id  TEXT    NOT NULL REFERENCES spec_draft(id),
  project_id     TEXT    NOT NULL REFERENCES project(id),
  session_id     TEXT    NOT NULL REFERENCES session(id),
  turn           INTEGER NOT NULL CHECK (turn >= 1),
  outcome        TEXT    NOT NULL CHECK (outcome IN ('passed','failed','missing')),
  bytes          INTEGER CHECK (bytes IS NULL OR bytes >= 0),
  content        BLOB    CHECK (content IS NULL OR length(content) <= 262144),
  content_hash   TEXT    CHECK (content_hash IS NULL OR length(content_hash) = 71),
  parser_version INTEGER NOT NULL CHECK (parser_version >= 1),
  issues         TEXT    NOT NULL CHECK (json_valid(issues) AND json_type(issues) = 'array'),
  decisions_open INTEGER NOT NULL DEFAULT 0 CHECK (decisions_open >= 0),
  created_at     INTEGER NOT NULL,
  PRIMARY KEY (attempt_id, round),
  UNIQUE (session_id, turn),
  CHECK ((outcome = 'missing') = (bytes IS NULL)),
  CHECK (outcome <> 'passed' OR content IS NOT NULL),
  CHECK ((content IS NULL) = (content_hash IS NULL))
) STRICT, WITHOUT ROWID;

INSERT INTO spec_draft_lint_round_v36 (attempt_id, round, spec_draft_id, project_id, session_id, turn, outcome,
  bytes, content, content_hash, parser_version, issues, decisions_open, created_at)
SELECT attempt_id, round, spec_draft_id, project_id, session_id, turn, outcome,
  bytes, content, content_hash, parser_version, issues, decisions_open, created_at
FROM spec_draft_lint_round;

DROP TABLE spec_draft_lint_round;
ALTER TABLE spec_draft_lint_round_v36 RENAME TO spec_draft_lint_round;
CREATE INDEX spec_draft_lint_round_draft_idx ON spec_draft_lint_round (spec_draft_id, created_at);
