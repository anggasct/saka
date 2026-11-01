-- Initial schema. The contract for every table lives in the database reference docs.

CREATE TABLE repo (
  id             TEXT    PRIMARY KEY,
  integration_id TEXT    NOT NULL,
  provider       TEXT    NOT NULL CHECK (provider IN ('github')),
  full_name      TEXT    NOT NULL,
  url            TEXT    NOT NULL,
  default_branch TEXT    NOT NULL,
  created_at     INTEGER NOT NULL,
  updated_at     INTEGER NOT NULL,
  UNIQUE (integration_id, full_name)
) STRICT;

CREATE TABLE project (
  id                TEXT    PRIMARY KEY,
  name              TEXT    NOT NULL CHECK (length(name) BETWEEN 1 AND 80),
  workspace_repo_id TEXT    NOT NULL REFERENCES repo(id),
  project_path      TEXT    NOT NULL,
  base_branch       TEXT    NOT NULL,
  config_commit     TEXT    NOT NULL CHECK (length(config_commit) = 40),
  autonomy          TEXT    NOT NULL DEFAULT 'standard'
                    CHECK (autonomy IN ('strict','standard','autonomous')),
  workflow_preset   TEXT    NOT NULL DEFAULT 'full_loop'
                    CHECK (workflow_preset IN ('full_loop','review_only','build_only','custom')),
  created_at        INTEGER NOT NULL,
  updated_at        INTEGER NOT NULL,
  UNIQUE (workspace_repo_id, project_path)
) STRICT;
CREATE INDEX project_workspace_repo_idx ON project (workspace_repo_id);

CREATE TABLE project_repo (
  project_id TEXT    NOT NULL REFERENCES project(id),
  repo_id    TEXT    NOT NULL REFERENCES repo(id),
  name       TEXT    NOT NULL,
  is_default INTEGER NOT NULL CHECK (is_default IN (0,1)),
  path_scope TEXT    NOT NULL DEFAULT '["**"]' CHECK (json_valid(path_scope) AND json_type(path_scope) = 'array'),
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  PRIMARY KEY (project_id, repo_id),
  UNIQUE (project_id, name)
) STRICT;
CREATE UNIQUE INDEX project_repo_one_default ON project_repo (project_id) WHERE is_default = 1;
CREATE INDEX project_repo_repo_idx ON project_repo (repo_id);

CREATE TABLE spec (
  id                   TEXT    PRIMARY KEY,
  project_id           TEXT    NOT NULL REFERENCES project(id),
  path                 TEXT    NOT NULL,
  slug                 TEXT    NOT NULL,
  title                TEXT    NOT NULL,
  state                TEXT    NOT NULL CHECK (state IN ('draft','ready','approved','shipped','withdrawn')),
  origin               TEXT    NOT NULL DEFAULT 'manual' CHECK (origin IN ('manual','architect','intake')),
  priority             INTEGER NOT NULL DEFAULT 0,
  latest_revision_id   TEXT    NOT NULL,
  approved_revision_id TEXT,
  approved_at          INTEGER,
  shipped_at           INTEGER,
  created_at           INTEGER NOT NULL,
  updated_at           INTEGER NOT NULL,
  UNIQUE (project_id, path),
  UNIQUE (project_id, slug),
  CHECK ((state IN ('approved','shipped')) = (approved_revision_id IS NOT NULL))
) STRICT;
CREATE INDEX spec_project_state_idx ON spec (project_id, state);

CREATE TABLE spec_revision (
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
  UNIQUE (spec_id, content_hash)
) STRICT;
CREATE INDEX spec_revision_spec_idx ON spec_revision (spec_id, created_at);

CREATE TABLE work_item (
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
                   ('queued','awaiting_start','admitted','in_stage','blocked','taken_over','done','cancelled')),
  stage            TEXT    CHECK (stage IS NULL OR stage IN ('dev','review','fix','merge','sync')),
  stage_phase      TEXT    CHECK (stage_phase IS NULL OR stage_phase IN
                   ('preparing_environment','running_session','pushing','opening_change',
                    'awaiting_ci','awaiting_merge','merging','verifying','tearing_down')),
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
CREATE INDEX work_item_project_state_idx ON work_item (project_id, state);
CREATE INDEX work_item_spec_idx ON work_item (spec_id, step_index);
CREATE INDEX work_item_admission_idx ON work_item (state, priority, created_at);

CREATE TABLE stage_run (
  id           TEXT    PRIMARY KEY,
  work_item_id TEXT    NOT NULL REFERENCES work_item(id),
  project_id   TEXT    NOT NULL REFERENCES project(id),
  stage        TEXT    NOT NULL CHECK (stage IN ('dev','review','fix','merge','sync')),
  attempt      INTEGER NOT NULL CHECK (attempt >= 1),
  state        TEXT    NOT NULL CHECK (state IN ('pending','running','succeeded','failed','blocked','cancelled')),
  trigger      TEXT    NOT NULL CHECK (trigger IN ('auto','manual')),
  reason       TEXT    CHECK (reason IS NULL OR length(reason) BETWEEN 1 AND 500),
  entry_phase  TEXT    NOT NULL,
  head_sha     TEXT    CHECK (head_sha IS NULL OR length(head_sha) = 40),
  failure      TEXT    CHECK (failure IS NULL OR json_valid(failure)),
  started_at   INTEGER,
  ended_at     INTEGER,
  created_at   INTEGER NOT NULL,
  updated_at   INTEGER NOT NULL,
  UNIQUE (work_item_id, stage, attempt),
  CHECK (trigger = 'auto' OR reason IS NOT NULL)
) STRICT;
CREATE UNIQUE INDEX stage_run_one_active ON stage_run (work_item_id) WHERE state IN ('pending','running');

CREATE TABLE gate_result (
  id           TEXT    PRIMARY KEY,
  stage_run_id TEXT    NOT NULL REFERENCES stage_run(id),
  work_item_id TEXT    NOT NULL REFERENCES work_item(id),
  project_id   TEXT    NOT NULL REFERENCES project(id),
  source       TEXT    NOT NULL CHECK (source IN ('scm','saka')),
  head_sha     TEXT    NOT NULL CHECK (length(head_sha) = 40),
  outcome      TEXT    NOT NULL CHECK (outcome IN ('pending','passed','failed')),
  checks       TEXT    NOT NULL CHECK (json_valid(checks)),
  observed_at  INTEGER NOT NULL
) STRICT;
CREATE INDEX gate_result_work_item_idx ON gate_result (work_item_id, observed_at);

CREATE TABLE blocker (
  id           TEXT    PRIMARY KEY,
  work_item_id TEXT    NOT NULL REFERENCES work_item(id),
  stage_run_id TEXT    REFERENCES stage_run(id),
  project_id   TEXT    NOT NULL REFERENCES project(id),
  code         TEXT    NOT NULL CHECK (code IN (
                 'env_setup_failed','session_failed','session_lost','session_timeout','permission_expired',
                 'quota_exhausted','no_changes','ci_red','verdict_failed','review_rounds_exhausted',
                 'merge_conflict','push_rejected','change_closed','scm_unavailable','spec_unclear','dod_failed',
                 'hook_failed','budget_exhausted')),
  params       TEXT    NOT NULL CHECK (json_valid(params)),
  state        TEXT    NOT NULL CHECK (state IN ('open','resolved')),
  resolution   TEXT    CHECK (resolution IS NULL OR resolution IN ('retried','cancelled','auto')),
  opened_at    INTEGER NOT NULL,
  resolved_at  INTEGER,
  CHECK ((state = 'resolved') = (resolution IS NOT NULL AND resolved_at IS NOT NULL))
) STRICT;
CREATE UNIQUE INDEX blocker_one_open ON blocker (work_item_id) WHERE state = 'open';

CREATE TABLE change_request (
  id               TEXT    PRIMARY KEY,
  project_id       TEXT    NOT NULL REFERENCES project(id),
  work_item_id     TEXT    NOT NULL UNIQUE REFERENCES work_item(id),
  repo_id          TEXT    NOT NULL REFERENCES repo(id),
  number           INTEGER NOT NULL CHECK (number >= 1),
  url              TEXT    NOT NULL,
  head_branch      TEXT    NOT NULL,
  base_branch      TEXT    NOT NULL,
  head_sha         TEXT    NOT NULL CHECK (length(head_sha) = 40),
  state            TEXT    NOT NULL CHECK (state IN ('open','merged','closed')),
  merge_commit_sha TEXT    CHECK (merge_commit_sha IS NULL OR length(merge_commit_sha) = 40),
  merged_at        INTEGER,
  last_polled_at   INTEGER,
  created_at       INTEGER NOT NULL,
  updated_at       INTEGER NOT NULL,
  UNIQUE (repo_id, number),
  CHECK ((state = 'merged') = (merge_commit_sha IS NOT NULL))
) STRICT;
CREATE INDEX change_request_state_idx ON change_request (state);

CREATE TABLE ci_check (
  change_request_id TEXT    NOT NULL REFERENCES change_request(id),
  head_sha          TEXT    NOT NULL CHECK (length(head_sha) = 40),
  kind              TEXT    NOT NULL CHECK (kind IN ('check_run','commit_status')),
  name              TEXT    NOT NULL,
  status            TEXT    NOT NULL CHECK (status IN ('queued','in_progress','completed')),
  conclusion        TEXT    CHECK (conclusion IS NULL OR conclusion IN
                    ('success','failure','neutral','skipped','cancelled','timed_out','action_required','stale','error')),
  url               TEXT,
  observed_at       INTEGER NOT NULL,
  PRIMARY KEY (change_request_id, head_sha, kind, name),
  CHECK ((status = 'completed') = (conclusion IS NOT NULL))
) STRICT;

CREATE TABLE environment (
  id            TEXT    PRIMARY KEY,
  project_id    TEXT    NOT NULL REFERENCES project(id),
  work_item_id  TEXT    NOT NULL REFERENCES work_item(id),
  repo_id       TEXT    NOT NULL REFERENCES repo(id),
  root_path     TEXT    NOT NULL,
  worktree_path TEXT    NOT NULL,
  branch        TEXT    NOT NULL,
  base_sha      TEXT    CHECK (base_sha IS NULL OR length(base_sha) = 40),
  state         TEXT    NOT NULL CHECK (state IN ('preparing','ready','failed','torn_down')),
  created_at    INTEGER NOT NULL,
  updated_at    INTEGER NOT NULL,
  CHECK (state <> 'ready' OR base_sha IS NOT NULL)
) STRICT;
CREATE UNIQUE INDEX environment_one_live ON environment (work_item_id) WHERE state IN ('preparing','ready');

CREATE TABLE session (
  id               TEXT    PRIMARY KEY,
  project_id       TEXT    NOT NULL REFERENCES project(id),
  work_item_id     TEXT    NOT NULL REFERENCES work_item(id),
  stage_run_id     TEXT    NOT NULL REFERENCES stage_run(id),
  environment_id   TEXT    NOT NULL REFERENCES environment(id),
  cli              TEXT    NOT NULL CHECK (cli IN ('claude','codex','opencode')),
  cli_version      TEXT,
  cli_session_id   TEXT,
  model            TEXT,
  account          TEXT    NOT NULL,
  state            TEXT    NOT NULL CHECK (state IN
                   ('starting','running','waiting','idle','interrupted','lost','resumed','ended','failed')),
  runner_unit      TEXT    NOT NULL,
  socket_path      TEXT    NOT NULL,
  log_path         TEXT    NOT NULL,
  protocol_version INTEGER,
  last_offset      INTEGER NOT NULL DEFAULT 0 CHECK (last_offset >= 0),
  turns            INTEGER NOT NULL DEFAULT 0,
  followups        INTEGER NOT NULL DEFAULT 0,
  running_ms       INTEGER NOT NULL DEFAULT 0,
  cost_usd_micros  INTEGER,
  input_tokens     INTEGER,
  output_tokens    INTEGER,
  end_reason       TEXT    CHECK (end_reason IS NULL OR json_valid(end_reason)),
  started_at       INTEGER,
  ended_at         INTEGER,
  created_at       INTEGER NOT NULL,
  updated_at       INTEGER NOT NULL
) STRICT;
CREATE INDEX session_work_item_idx ON session (work_item_id);
CREATE INDEX session_state_idx ON session (state);
CREATE UNIQUE INDEX session_one_live ON session (work_item_id)
  WHERE state IN ('starting','running','waiting','idle','interrupted');

CREATE TABLE permission_request (
  id             TEXT    PRIMARY KEY,
  project_id     TEXT    NOT NULL REFERENCES project(id),
  work_item_id   TEXT    NOT NULL REFERENCES work_item(id),
  session_id     TEXT    NOT NULL REFERENCES session(id),
  cli_request_id TEXT    NOT NULL,
  tool           TEXT    NOT NULL,
  input          TEXT    NOT NULL CHECK (json_valid(input)),
  summary        TEXT    NOT NULL CHECK (length(summary) <= 200),
  state          TEXT    NOT NULL CHECK (state IN ('pending','allowed','denied','expired')),
  answer_source  TEXT,
  message        TEXT    CHECK (message IS NULL OR length(message) <= 500),
  requested_at   INTEGER NOT NULL,
  answered_at    INTEGER,
  delivered_at   INTEGER,
  UNIQUE (session_id, cli_request_id),
  CHECK ((state = 'pending') = (answer_source IS NULL))
) STRICT;
CREATE INDEX permission_request_pending_idx ON permission_request (state, requested_at);

CREATE TABLE event (
  seq            INTEGER PRIMARY KEY AUTOINCREMENT,
  id             TEXT    NOT NULL UNIQUE,
  kind           TEXT    NOT NULL,
  version        INTEGER NOT NULL CHECK (version >= 1),
  stream_type    TEXT    NOT NULL,
  stream_id      TEXT    NOT NULL,
  project_id     TEXT    REFERENCES project(id),
  actor          TEXT    NOT NULL,
  causation_id   TEXT,
  correlation_id TEXT    NOT NULL,
  occurred_at    INTEGER NOT NULL,
  payload        TEXT    NOT NULL CHECK (json_valid(payload))
) STRICT;
CREATE INDEX event_stream_idx  ON event (stream_type, stream_id, seq);
CREATE INDEX event_project_idx ON event (project_id, seq);

CREATE TABLE action_request (
  id              TEXT    PRIMARY KEY,
  actor           TEXT    NOT NULL,
  idempotency_key TEXT    NOT NULL,
  request_hash    TEXT    NOT NULL,
  action          TEXT    NOT NULL,
  target_type     TEXT    NOT NULL,
  target_id       TEXT,
  project_id      TEXT    REFERENCES project(id),
  reason          TEXT    CHECK (reason IS NULL OR length(reason) BETWEEN 1 AND 500),
  response_status INTEGER NOT NULL CHECK (response_status BETWEEN 200 AND 499),
  response_body   TEXT    NOT NULL CHECK (json_valid(response_body)),
  created_at      INTEGER NOT NULL,
  UNIQUE (actor, idempotency_key)
) STRICT;
CREATE INDEX action_request_target_idx ON action_request (target_type, target_id);

CREATE TABLE job (
  id              TEXT    PRIMARY KEY,
  kind            TEXT    NOT NULL,
  idempotency_key TEXT    NOT NULL UNIQUE,
  project_id      TEXT    REFERENCES project(id),
  payload         TEXT    NOT NULL CHECK (json_valid(payload)),
  state           TEXT    NOT NULL CHECK (state IN ('queued','leased','succeeded','failed','dead')),
  run_at          INTEGER NOT NULL,
  attempts        INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
  max_attempts    INTEGER NOT NULL CHECK (max_attempts >= 1),
  lease_owner     TEXT,
  lease_until     INTEGER,
  last_error      TEXT    CHECK (last_error IS NULL OR json_valid(last_error)),
  causation_id    TEXT,
  correlation_id  TEXT    NOT NULL,
  created_at      INTEGER NOT NULL,
  updated_at      INTEGER NOT NULL,
  CHECK ((state = 'leased') = (lease_owner IS NOT NULL AND lease_until IS NOT NULL))
) STRICT;
CREATE INDEX job_ready_idx ON job (state, run_at);
CREATE INDEX job_lease_idx ON job (state, lease_until);
