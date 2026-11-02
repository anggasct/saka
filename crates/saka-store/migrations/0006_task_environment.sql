ALTER TABLE environment ADD COLUMN recipe_json TEXT;
ALTER TABLE environment ADD COLUMN toolchains_json TEXT NOT NULL DEFAULT '[]';
ALTER TABLE environment ADD COLUMN port_start INTEGER
  CHECK (port_start IS NULL OR port_start BETWEEN 1024 AND 65535);
ALTER TABLE environment ADD COLUMN port_count INTEGER
  CHECK (port_count IS NULL OR port_count BETWEEN 1 AND 100);
ALTER TABLE environment ADD COLUMN recipe_sha256 TEXT
  CHECK (recipe_sha256 IS NULL OR length(recipe_sha256) = 64);

CREATE UNIQUE INDEX environment_port_live ON environment (port_start)
  WHERE port_start IS NOT NULL AND state IN ('preparing','ready','teardown_failed');

CREATE TABLE blocker_v6 (
  id           TEXT    PRIMARY KEY,
  work_item_id TEXT    NOT NULL REFERENCES work_item(id),
  stage_run_id TEXT    REFERENCES stage_run(id),
  project_id   TEXT    NOT NULL REFERENCES project(id),
  code         TEXT    NOT NULL CHECK (code IN (
                 'env_setup_failed','env_recipe_invalid','session_failed','session_lost','session_timeout',
                 'permission_expired','quota_exhausted','no_changes','ci_red','verdict_failed',
                 'review_rounds_exhausted','merge_conflict','push_rejected','change_closed','scm_unavailable',
                 'spec_unclear','dod_failed','hook_failed','budget_exhausted')),
  params       TEXT    NOT NULL CHECK (json_valid(params)),
  state        TEXT    NOT NULL CHECK (state IN ('open','resolved')),
  resolution   TEXT    CHECK (resolution IS NULL OR resolution IN ('retried','cancelled','auto')),
  opened_at    INTEGER NOT NULL,
  resolved_at  INTEGER,
  CHECK ((state = 'resolved') = (resolution IS NOT NULL AND resolved_at IS NOT NULL))
) STRICT;

INSERT INTO blocker_v6 (id, work_item_id, stage_run_id, project_id, code, params, state, resolution,
  opened_at, resolved_at)
SELECT id, work_item_id, stage_run_id, project_id, code, params, state, resolution, opened_at, resolved_at
FROM blocker;

DROP TABLE blocker;
ALTER TABLE blocker_v6 RENAME TO blocker;
CREATE UNIQUE INDEX blocker_one_open ON blocker (work_item_id) WHERE state = 'open';
