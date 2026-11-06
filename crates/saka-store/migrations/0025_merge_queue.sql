CREATE TABLE work_item_v25 (
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
                    'awaiting_ci','awaiting_merge','merging','merging_docs','verifying','tearing_down','awaiting_trigger','awaiting_review','posting_review','reviewed',
                    'queued','integrating')),
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

INSERT INTO work_item_v25 (id, project_id, origin, spec_id, spec_revision_id, step_id, step_index, task_key, work,
  repo_id, branch, state, stage, stage_phase, priority, version, admitted_at, done_at, created_at, updated_at)
SELECT id, project_id, origin, spec_id, spec_revision_id, step_id, step_index, task_key, work, repo_id, branch,
  state, stage, stage_phase, priority, version, admitted_at, done_at, created_at, updated_at
FROM work_item;

DROP TABLE work_item;
ALTER TABLE work_item_v25 RENAME TO work_item;
CREATE UNIQUE INDEX work_item_branch_uq ON work_item (repo_id, branch) WHERE origin = 'spec_step';
CREATE INDEX work_item_project_state_idx ON work_item (project_id, state);
CREATE INDEX work_item_spec_idx ON work_item (spec_id, step_index);
CREATE INDEX work_item_admission_idx ON work_item (state, priority, created_at);

ALTER TABLE stage_run ADD COLUMN queued_at INTEGER;
