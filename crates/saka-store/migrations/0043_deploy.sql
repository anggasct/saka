ALTER TABLE project ADD COLUMN verify TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(verify));

CREATE TABLE deployment (
  id            TEXT    PRIMARY KEY,
  project_id    TEXT    NOT NULL REFERENCES project(id),
  repo_id       TEXT    NOT NULL REFERENCES repo(id),
  environment   TEXT    NOT NULL CHECK (length(environment) BETWEEN 1 AND 32),
  branch        TEXT    NOT NULL,
  sha           TEXT    CHECK (sha IS NULL OR length(sha) = 40),
  previous_sha  TEXT    CHECK (previous_sha IS NULL OR length(previous_sha) = 40),
  cause         TEXT    NOT NULL CHECK (cause IN ('merge','redeploy','retry')),
  state         TEXT    NOT NULL CHECK (state IN
                ('queued','deploying','awaiting_ready','verifying','succeeded','failed','cancelled')),
  phase         TEXT    CHECK (phase IS NULL OR phase IN ('deploy','ready','verify')),
  error         TEXT    CHECK (error IS NULL OR json_valid(error)),
  action        TEXT    NOT NULL CHECK (action IN ('run','run_host','webhook')),
  target_url    TEXT,
  verification  TEXT    CHECK (verification IS NULL OR json_valid(verification)),
  actor         TEXT,
  reason        TEXT,
  queued_at     INTEGER NOT NULL,
  started_at    INTEGER,
  finished_at   INTEGER,
  updated_at    INTEGER NOT NULL,
  CHECK ((state = 'failed') = (error IS NOT NULL)),
  CHECK ((state IN ('succeeded','failed','cancelled')) = (finished_at IS NOT NULL))
) STRICT;

CREATE UNIQUE INDEX deployment_one_queued ON deployment (project_id, environment) WHERE state = 'queued';
CREATE UNIQUE INDEX deployment_one_active ON deployment (project_id, environment)
  WHERE state IN ('deploying','awaiting_ready','verifying');
CREATE INDEX deployment_environment_idx ON deployment (project_id, environment, queued_at DESC, id DESC);
CREATE INDEX deployment_list_idx ON deployment (project_id, queued_at DESC, id DESC);

CREATE TABLE deployment_check (
  deployment_id TEXT    NOT NULL REFERENCES deployment(id),
  phase         TEXT    NOT NULL CHECK (phase IN ('deploy','ready','verify')),
  ordinal       INTEGER NOT NULL CHECK (ordinal >= 0),
  command       TEXT,
  state         TEXT    NOT NULL CHECK (state IN ('passed','failed','timed_out','skipped')),
  exit_code     INTEGER,
  http_status   INTEGER,
  attempts      INTEGER NOT NULL CHECK (attempts >= 0),
  duration_ms   INTEGER NOT NULL CHECK (duration_ms >= 0),
  output_tail   TEXT    NOT NULL CHECK (length(CAST(output_tail AS BLOB)) <= 8192),
  log           TEXT,
  started_at    INTEGER NOT NULL,
  finished_at   INTEGER NOT NULL,
  PRIMARY KEY (deployment_id, phase, ordinal)
) STRICT;

CREATE TABLE deployment_merge (
  deployment_id     TEXT NOT NULL REFERENCES deployment(id),
  change_request_id TEXT NOT NULL REFERENCES change_request(id),
  PRIMARY KEY (deployment_id, change_request_id)
) STRICT;

CREATE TABLE deployment_task (
  deployment_id TEXT NOT NULL REFERENCES deployment(id),
  work_item_id  TEXT NOT NULL REFERENCES work_item(id),
  PRIMARY KEY (deployment_id, work_item_id)
) STRICT;

CREATE TABLE deploy_gate (
  work_item_id  TEXT    NOT NULL REFERENCES work_item(id),
  environment   TEXT    NOT NULL,
  state         TEXT    NOT NULL CHECK (state IN ('pending','passed','failed','accepted','skipped')),
  deployment_id TEXT    REFERENCES deployment(id),
  reason        TEXT,
  created_at    INTEGER NOT NULL,
  updated_at    INTEGER NOT NULL,
  PRIMARY KEY (work_item_id, environment)
) STRICT;
CREATE INDEX deploy_gate_deployment_idx ON deploy_gate (deployment_id) WHERE deployment_id IS NOT NULL;

CREATE TABLE blocker_v43 (
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
                 'regression_not_reproduced','revert_conflict','deploy_unverified')),
  params       TEXT    NOT NULL CHECK (json_valid(params)),
  state        TEXT    NOT NULL CHECK (state IN ('open','resolved')),
  resolution   TEXT    CHECK (resolution IS NULL OR resolution IN ('retried','cancelled','auto','taken_over','reverted','accepted')),
  opened_at    INTEGER NOT NULL,
  resolved_at  INTEGER,
  CHECK ((state = 'resolved') = (resolution IS NOT NULL AND resolved_at IS NOT NULL))
) STRICT;

INSERT INTO blocker_v43 (id, work_item_id, stage_run_id, project_id, code, params, state, resolution,
  opened_at, resolved_at)
SELECT id, work_item_id, stage_run_id, project_id, code, params, state, resolution, opened_at, resolved_at
FROM blocker;

DROP TABLE blocker;
ALTER TABLE blocker_v43 RENAME TO blocker;
CREATE UNIQUE INDEX blocker_one_open ON blocker (work_item_id) WHERE state = 'open';
