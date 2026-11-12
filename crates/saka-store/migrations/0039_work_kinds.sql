CREATE TABLE spec_v39 (
  id                   TEXT    PRIMARY KEY,
  project_id           TEXT    NOT NULL REFERENCES project(id),
  path                 TEXT    NOT NULL,
  slug                 TEXT    NOT NULL,
  title                TEXT    NOT NULL,
  state                TEXT    NOT NULL CHECK (state IN ('draft','ready','approved','shipped','withdrawn')),
  origin               TEXT    NOT NULL DEFAULT 'manual' CHECK (origin IN ('manual','architect','intake','quick')),
  priority             INTEGER NOT NULL DEFAULT 0,
  latest_revision_id   TEXT    NOT NULL,
  approved_revision_id TEXT,
  approved_at          INTEGER,
  shipped_at           INTEGER,
  created_at           INTEGER NOT NULL,
  updated_at           INTEGER NOT NULL,
  UNIQUE (project_id, path),
  UNIQUE (project_id, slug),
  CHECK (state NOT IN ('approved','shipped') OR approved_revision_id IS NOT NULL),
  CHECK (state NOT IN ('draft','ready') OR approved_revision_id IS NULL)
) STRICT;

INSERT INTO spec_v39 (id, project_id, path, slug, title, state, origin, priority, latest_revision_id,
  approved_revision_id, approved_at, shipped_at, created_at, updated_at)
SELECT id, project_id, path, slug, title, state, origin, priority, latest_revision_id,
  approved_revision_id, approved_at, shipped_at, created_at, updated_at
FROM spec;

DROP TABLE spec;
ALTER TABLE spec_v39 RENAME TO spec;
CREATE INDEX spec_project_state_idx ON spec (project_id, state);

ALTER TABLE spec ADD COLUMN kind TEXT NOT NULL DEFAULT 'feature'
  CHECK (kind IN ('feature','bugfix','refactor','chore','security_patch'));

ALTER TABLE work_item ADD COLUMN kind TEXT
  CHECK (kind IS NULL OR kind IN ('feature','bugfix','refactor','chore','security_patch'));
UPDATE work_item SET kind = 'feature' WHERE origin = 'spec_step';

CREATE TABLE regression_check (
  id            TEXT    PRIMARY KEY,
  work_item_id  TEXT    NOT NULL REFERENCES work_item(id),
  stage_run_id  TEXT    NOT NULL REFERENCES stage_run(id),
  project_id    TEXT    NOT NULL REFERENCES project(id),
  attempt       INTEGER NOT NULL CHECK (attempt >= 1),
  commit_sha    TEXT    NOT NULL CHECK (length(commit_sha) = 40),
  base_sha      TEXT    NOT NULL CHECK (length(base_sha) = 40),
  outcome       TEXT    CHECK (outcome IS NULL OR outcome IN ('reproduced','not_reproduced','invalid_paths','error')),
  checks        TEXT    NOT NULL DEFAULT '[]' CHECK (json_valid(checks)),
  paths         TEXT    NOT NULL DEFAULT '[]' CHECK (json_valid(paths)),
  requested_at  INTEGER NOT NULL,
  observed_at   INTEGER,
  UNIQUE (stage_run_id, attempt),
  CHECK ((outcome IS NULL) = (observed_at IS NULL))
) STRICT;
CREATE INDEX regression_check_work_item_idx ON regression_check (work_item_id);

CREATE TABLE blocker_v39 (
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
                 'docs_merge_failed','branch_moved','decision_expired','change_behind_base',
                 'regression_not_reproduced')),
  params       TEXT    NOT NULL CHECK (json_valid(params)),
  state        TEXT    NOT NULL CHECK (state IN ('open','resolved')),
  resolution   TEXT    CHECK (resolution IS NULL OR resolution IN ('retried','cancelled','auto','taken_over')),
  opened_at    INTEGER NOT NULL,
  resolved_at  INTEGER,
  CHECK ((state = 'resolved') = (resolution IS NOT NULL AND resolved_at IS NOT NULL))
) STRICT;

INSERT INTO blocker_v39 (id, work_item_id, stage_run_id, project_id, code, params, state, resolution,
  opened_at, resolved_at)
SELECT id, work_item_id, stage_run_id, project_id, code, params, state, resolution, opened_at, resolved_at
FROM blocker;

DROP TABLE blocker;
ALTER TABLE blocker_v39 RENAME TO blocker;
CREATE UNIQUE INDEX blocker_one_open ON blocker (work_item_id) WHERE state = 'open';
