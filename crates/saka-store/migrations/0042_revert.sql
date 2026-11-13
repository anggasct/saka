CREATE TABLE work_item_v42 (
  id               TEXT    PRIMARY KEY,
  project_id       TEXT    NOT NULL REFERENCES project(id),
  origin           TEXT    NOT NULL CHECK (origin IN ('spec_step','external_change','revert')),
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
                   ('preparing_environment','reverting','running_session','pushing','opening_change',
                    'awaiting_ci','awaiting_merge','merging','merging_docs','verifying','tearing_down','awaiting_trigger','awaiting_review','posting_review','reviewed',
                    'queued','integrating')),
  priority         INTEGER NOT NULL DEFAULT 0,
  version          INTEGER NOT NULL CHECK (version >= 1),
  admitted_at      INTEGER,
  done_at          INTEGER,
  created_at       INTEGER NOT NULL,
  updated_at       INTEGER NOT NULL,
  start_approved_at INTEGER,
  kind             TEXT    CHECK (kind IS NULL OR kind IN ('feature','bugfix','refactor','chore','security_patch')),
  revert_of        TEXT    REFERENCES work_item(id),
  revert_reason    TEXT    CHECK (revert_reason IS NULL OR length(revert_reason) BETWEEN 1 AND 500),
  CHECK ((origin = 'spec_step') = (spec_id IS NOT NULL AND spec_revision_id IS NOT NULL
                                   AND step_id IS NOT NULL AND step_index IS NOT NULL)),
  CHECK ((origin = 'revert') = (revert_of IS NOT NULL AND revert_reason IS NOT NULL)),
  CHECK ((state = 'in_stage') = (stage IS NOT NULL AND stage_phase IS NOT NULL)),
  UNIQUE (project_id, task_key)
) STRICT;

INSERT INTO work_item_v42 (id, project_id, origin, spec_id, spec_revision_id, step_id, step_index, task_key, work,
  repo_id, branch, state, stage, stage_phase, priority, version, admitted_at, done_at, created_at, updated_at,
  start_approved_at, kind)
SELECT id, project_id, origin, spec_id, spec_revision_id, step_id, step_index, task_key, work, repo_id, branch,
  state, stage, stage_phase, priority, version, admitted_at, done_at, created_at, updated_at,
  start_approved_at, kind
FROM work_item;

DROP TABLE work_item;
ALTER TABLE work_item_v42 RENAME TO work_item;
CREATE UNIQUE INDEX work_item_branch_uq ON work_item (repo_id, branch) WHERE origin = 'spec_step';
CREATE INDEX work_item_project_state_idx ON work_item (project_id, state);
CREATE INDEX work_item_spec_idx ON work_item (spec_id, step_index);
CREATE INDEX work_item_admission_idx ON work_item (state, priority, created_at);
CREATE UNIQUE INDEX work_item_one_live_revert ON work_item (revert_of)
  WHERE revert_of IS NOT NULL AND state NOT IN ('done','cancelled','superseded');

CREATE TABLE blocker_v42 (
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
                 'regression_not_reproduced','revert_conflict')),
  params       TEXT    NOT NULL CHECK (json_valid(params)),
  state        TEXT    NOT NULL CHECK (state IN ('open','resolved')),
  resolution   TEXT    CHECK (resolution IS NULL OR resolution IN ('retried','cancelled','auto','taken_over','reverted')),
  opened_at    INTEGER NOT NULL,
  resolved_at  INTEGER,
  CHECK ((state = 'resolved') = (resolution IS NOT NULL AND resolved_at IS NOT NULL))
) STRICT;

INSERT INTO blocker_v42 (id, work_item_id, stage_run_id, project_id, code, params, state, resolution,
  opened_at, resolved_at)
SELECT id, work_item_id, stage_run_id, project_id, code, params, state, resolution, opened_at, resolved_at
FROM blocker;

DROP TABLE blocker;
ALTER TABLE blocker_v42 RENAME TO blocker;
CREATE UNIQUE INDEX blocker_one_open ON blocker (work_item_id) WHERE state = 'open';
