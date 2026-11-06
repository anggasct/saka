CREATE TABLE work_item_v22 (
  id               TEXT    PRIMARY KEY,
  project_id       TEXT    NOT NULL REFERENCES project(id),
  origin           TEXT    NOT NULL CHECK (origin IN ('spec_step','external_change')),
  spec_id          TEXT    REFERENCES spec(id),
  spec_revision_id TEXT    REFERENCES spec_revision(id),
  step_id          TEXT,
  step_index       INTEGER CHECK (step_index IS NULL OR step_index >= 1),
  task_key         TEXT    NOT NULL,
  work             TEXT    NOT NULL,
  repo_id          TEXT    NOT NULL REFERENCES repo(id),
  branch           TEXT    NOT NULL,
  state            TEXT    NOT NULL CHECK (state IN
                   ('queued','awaiting_start','admitted','in_stage','blocked','taken_over','done','cancelled','superseded')),
  stage            TEXT    CHECK (stage IS NULL OR stage IN ('dev','review','fix','merge','sync')),
  stage_phase      TEXT    CHECK (stage_phase IS NULL OR stage_phase IN
                   ('preparing_environment','running_session','pushing','opening_change',
                    'awaiting_ci','awaiting_merge','merging','merging_docs','verifying','tearing_down','awaiting_trigger','awaiting_review','posting_review','reviewed')),
  priority         INTEGER NOT NULL DEFAULT 0,
  version          INTEGER NOT NULL CHECK (version >= 1),
  admitted_at      INTEGER,
  done_at          INTEGER,
  created_at       INTEGER NOT NULL,
  updated_at       INTEGER NOT NULL,
  CHECK ((origin = 'spec_step') = (spec_id IS NOT NULL AND spec_revision_id IS NOT NULL
                                   AND step_id IS NOT NULL AND step_index IS NOT NULL)),
  CHECK ((state = 'in_stage') = (stage IS NOT NULL AND stage_phase IS NOT NULL)),
  UNIQUE (project_id, task_key)
) STRICT;

INSERT INTO work_item_v22 (id, project_id, origin, spec_id, spec_revision_id, step_id, step_index, task_key, work,
  repo_id, branch, state, stage, stage_phase, priority, version, admitted_at, done_at, created_at, updated_at)
SELECT id, project_id, origin, spec_id, spec_revision_id, step_id, step_index, task_key, work, repo_id, branch,
  state, stage, stage_phase, priority, version, admitted_at, done_at, created_at, updated_at
FROM work_item;

DROP TABLE work_item;
ALTER TABLE work_item_v22 RENAME TO work_item;
CREATE UNIQUE INDEX work_item_branch_uq ON work_item (repo_id, branch) WHERE origin = 'spec_step';
CREATE INDEX work_item_project_state_idx ON work_item (project_id, state);
CREATE INDEX work_item_spec_idx ON work_item (spec_id, step_index);
CREATE INDEX work_item_admission_idx ON work_item (state, priority, created_at);

CREATE TABLE review (
  id           TEXT    PRIMARY KEY,
  work_item_id TEXT    NOT NULL REFERENCES work_item(id),
  stage_run_id TEXT    NOT NULL UNIQUE REFERENCES stage_run(id),
  session_id   TEXT    NOT NULL,
  head_sha     TEXT    NOT NULL CHECK (length(head_sha) = 40),
  outcome      TEXT    NOT NULL CHECK (outcome IN ('approved','changes_requested','spec_unclear')),
  basis        TEXT    NOT NULL CHECK (basis IN ('step','spec','none')),
  summary      TEXT    NOT NULL CHECK (length(summary) BETWEEN 1 AND 4000),
  findings     TEXT    NOT NULL DEFAULT '[]' CHECK (json_valid(findings)),
  questions    TEXT    NOT NULL DEFAULT '[]' CHECK (json_valid(questions)),
  posted_event TEXT    CHECK (posted_event IS NULL OR posted_event IN ('approve','request_changes','comment')),
  posted_url   TEXT,
  posted_at    INTEGER,
  created_at   INTEGER NOT NULL,
  CHECK ((posted_event IS NULL) = (posted_at IS NULL))
) STRICT;
CREATE INDEX review_work_item_idx ON review (work_item_id, created_at);

CREATE TABLE environment_v22 (
  id                    TEXT    PRIMARY KEY,
  project_id            TEXT    NOT NULL REFERENCES project(id),
  work_item_id          TEXT    REFERENCES work_item(id),
  spec_draft_id         TEXT    REFERENCES spec_draft(id),
  spec_draft_attempt_id TEXT    REFERENCES spec_draft_attempt(id),
  repo_id               TEXT    NOT NULL REFERENCES repo(id),
  root_path             TEXT    NOT NULL,
  worktree_path         TEXT    NOT NULL,
  branch                TEXT,
  base_sha              TEXT    CHECK (base_sha IS NULL OR length(base_sha) = 40),
  state                 TEXT    NOT NULL CHECK (state IN ('preparing','ready','failed','teardown_failed','torn_down')),
  created_at            INTEGER NOT NULL,
  updated_at            INTEGER NOT NULL,
  recipe_json           TEXT,
  toolchains_json       TEXT    NOT NULL DEFAULT '[]',
  port_start            INTEGER CHECK (port_start IS NULL OR port_start BETWEEN 1024 AND 65535),
  port_count            INTEGER CHECK (port_count IS NULL OR port_count BETWEEN 1 AND 100),
  recipe_sha256         TEXT    CHECK (recipe_sha256 IS NULL OR length(recipe_sha256) = 64),
  path_scope_json       TEXT    NOT NULL DEFAULT '["**"]'
                        CHECK (json_valid(path_scope_json) AND json_type(path_scope_json) = 'array'),
  docs_json             TEXT    CHECK (docs_json IS NULL OR json_valid(docs_json)),
  CHECK (state <> 'ready' OR base_sha IS NOT NULL),
  CHECK ((work_item_id IS NOT NULL AND spec_draft_id IS NULL AND spec_draft_attempt_id IS NULL)
      OR (work_item_id IS NULL AND spec_draft_id IS NOT NULL AND spec_draft_attempt_id IS NOT NULL)),
  CHECK (spec_draft_id IS NULL OR (recipe_json IS NULL AND recipe_sha256 IS NULL
                                   AND port_start IS NULL AND port_count IS NULL))
) STRICT;

INSERT INTO environment_v22 (id, project_id, work_item_id, spec_draft_id, spec_draft_attempt_id, repo_id, root_path,
  worktree_path, branch, base_sha, state, created_at, updated_at, recipe_json, toolchains_json, port_start,
  port_count, recipe_sha256, path_scope_json, docs_json)
SELECT id, project_id, work_item_id, spec_draft_id, spec_draft_attempt_id, repo_id, root_path,
  worktree_path, branch, base_sha, state, created_at, updated_at, recipe_json, toolchains_json, port_start,
  port_count, recipe_sha256, path_scope_json, docs_json
FROM environment;

DROP TABLE environment;
ALTER TABLE environment_v22 RENAME TO environment;
CREATE UNIQUE INDEX environment_one_live ON environment (work_item_id) WHERE state IN ('preparing','ready');
CREATE UNIQUE INDEX environment_one_live_draft ON environment (spec_draft_id) WHERE state IN ('preparing','ready');
CREATE UNIQUE INDEX environment_port_live ON environment (port_start)
  WHERE port_start IS NOT NULL AND state IN ('preparing','ready','teardown_failed');
