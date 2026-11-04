CREATE TABLE change_request_v18 (
  id               TEXT    PRIMARY KEY,
  project_id       TEXT    NOT NULL REFERENCES project(id),
  work_item_id     TEXT    NOT NULL REFERENCES work_item(id),
  role             TEXT    NOT NULL CHECK (role IN ('code','docs')),
  repo_id          TEXT    NOT NULL REFERENCES repo(id),
  number           INTEGER NOT NULL CHECK (number >= 1),
  url              TEXT    NOT NULL,
  head_branch      TEXT    NOT NULL,
  base_branch      TEXT    NOT NULL,
  head_sha         TEXT    NOT NULL CHECK (length(head_sha) = 40),
  state            TEXT    NOT NULL CHECK (state IN ('open','merged','closed')),
  merge_commit_sha TEXT    CHECK (merge_commit_sha IS NULL OR length(merge_commit_sha) = 40),
  merged_at        INTEGER,
  skipped_at       INTEGER,
  last_polled_at   INTEGER,
  created_at       INTEGER NOT NULL,
  updated_at       INTEGER NOT NULL,
  UNIQUE (work_item_id, role),
  UNIQUE (repo_id, number),
  CHECK ((state = 'merged') = (merge_commit_sha IS NOT NULL)),
  CHECK (skipped_at IS NULL OR (role = 'docs' AND state = 'closed'))
) STRICT;

INSERT INTO change_request_v18 (id, project_id, work_item_id, role, repo_id, number, url, head_branch,
  base_branch, head_sha, state, merge_commit_sha, merged_at, skipped_at, last_polled_at, created_at, updated_at)
SELECT id, project_id, work_item_id, 'code', repo_id, number, url, head_branch, base_branch, head_sha, state,
  merge_commit_sha, merged_at, NULL, last_polled_at, created_at, updated_at
FROM change_request;

DROP TABLE change_request;
ALTER TABLE change_request_v18 RENAME TO change_request;
CREATE INDEX change_request_state_idx ON change_request (state);
CREATE INDEX change_request_head_idx ON change_request (repo_id, head_sha);

CREATE TABLE work_item_v18 (
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
                    'awaiting_ci','awaiting_merge','merging','merging_docs','verifying','tearing_down')),
  priority         INTEGER NOT NULL DEFAULT 0,
  version          INTEGER NOT NULL CHECK (version >= 1),
  admitted_at      INTEGER,
  done_at          INTEGER,
  created_at       INTEGER NOT NULL,
  updated_at       INTEGER NOT NULL,
  CHECK ((origin = 'spec_step') = (spec_id IS NOT NULL AND spec_revision_id IS NOT NULL
                                   AND step_id IS NOT NULL AND step_index IS NOT NULL)),
  CHECK ((state = 'in_stage') = (stage IS NOT NULL AND stage_phase IS NOT NULL)),
  UNIQUE (project_id, task_key),
  UNIQUE (repo_id, branch)
) STRICT;

INSERT INTO work_item_v18 (id, project_id, origin, spec_id, spec_revision_id, step_id, step_index, task_key, work,
  repo_id, branch, state, stage, stage_phase, priority, version, admitted_at, done_at, created_at, updated_at)
SELECT id, project_id, origin, spec_id, spec_revision_id, step_id, step_index, task_key, work, repo_id, branch,
  state, stage, stage_phase, priority, version, admitted_at, done_at, created_at, updated_at
FROM work_item;

DROP TABLE work_item;
ALTER TABLE work_item_v18 RENAME TO work_item;
CREATE INDEX work_item_project_state_idx ON work_item (project_id, state);
CREATE INDEX work_item_spec_idx ON work_item (spec_id, step_index);
CREATE INDEX work_item_admission_idx ON work_item (state, priority, created_at);

ALTER TABLE stage_run ADD COLUMN docs_head_sha TEXT CHECK (docs_head_sha IS NULL OR length(docs_head_sha) = 40);

CREATE TABLE blocker_v18 (
  id           TEXT    PRIMARY KEY,
  work_item_id TEXT    NOT NULL REFERENCES work_item(id),
  stage_run_id TEXT    REFERENCES stage_run(id),
  project_id   TEXT    NOT NULL REFERENCES project(id),
  code         TEXT    NOT NULL CHECK (code IN (
                 'env_setup_failed','env_recipe_invalid','toolchain_unpinned','toolchain_unavailable',
                 'secret_missing','secret_unreadable',
                 'session_failed','session_lost','session_timeout','permission_expired','quota_exhausted',
                 'no_changes','ci_red','verdict_failed','review_rounds_exhausted','merge_conflict',
                 'push_rejected','change_closed','scm_unavailable','spec_unclear','dod_failed','hook_failed',
                 'budget_exhausted','path_scope_violation',
                 'user_link_required','user_token_invalid','user_access_denied',
                 'docs_merge_failed')),
  params       TEXT    NOT NULL CHECK (json_valid(params)),
  state        TEXT    NOT NULL CHECK (state IN ('open','resolved')),
  resolution   TEXT    CHECK (resolution IS NULL OR resolution IN ('retried','cancelled','auto')),
  opened_at    INTEGER NOT NULL,
  resolved_at  INTEGER,
  CHECK ((state = 'resolved') = (resolution IS NOT NULL AND resolved_at IS NOT NULL))
) STRICT;

INSERT INTO blocker_v18 (id, work_item_id, stage_run_id, project_id, code, params, state, resolution,
  opened_at, resolved_at)
SELECT id, work_item_id, stage_run_id, project_id, code, params, state, resolution, opened_at, resolved_at
FROM blocker;

DROP TABLE blocker;
ALTER TABLE blocker_v18 RENAME TO blocker;
CREATE UNIQUE INDEX blocker_one_open ON blocker (work_item_id) WHERE state = 'open';
