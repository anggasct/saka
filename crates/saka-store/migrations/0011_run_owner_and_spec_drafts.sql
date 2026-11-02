CREATE TABLE spec_draft (
  id                 TEXT    PRIMARY KEY,
  project_id         TEXT    NOT NULL REFERENCES project(id),
  path               TEXT    NOT NULL CHECK (length(path) BETWEEN 1 AND 1024),
  slug               TEXT    NOT NULL,
  idea               TEXT    NOT NULL CHECK (length(idea) BETWEEN 1 AND 8000),
  locale             TEXT    NOT NULL CHECK (locale IN ('en','id')),
  context_paths      TEXT    NOT NULL CHECK (json_valid(context_paths) AND json_type(context_paths) = 'array'
                                             AND json_array_length(context_paths) <= 16),
  state              TEXT    NOT NULL CHECK (state IN
                     ('queued','preparing','drafting','committing','succeeded','failed','cancelled')),
  attempt            INTEGER NOT NULL DEFAULT 0 CHECK (attempt >= 0),
  current_attempt_id TEXT,
  retries            INTEGER NOT NULL DEFAULT 0 CHECK (retries BETWEEN 0 AND 3),
  queued_at          INTEGER NOT NULL,
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
  CHECK (state IN ('queued','cancelled') OR attempt = retries + 1)
) STRICT;
CREATE INDEX spec_draft_project_idx ON spec_draft (project_id, created_at);
CREATE INDEX spec_draft_state_idx ON spec_draft (state, queued_at);
CREATE UNIQUE INDEX spec_draft_one_live_path ON spec_draft (project_id, path)
  WHERE state IN ('queued','preparing','drafting','committing','failed');

CREATE TABLE spec_draft_attempt (
  id              TEXT    PRIMARY KEY,
  spec_draft_id   TEXT    NOT NULL REFERENCES spec_draft(id),
  project_id      TEXT    NOT NULL REFERENCES project(id),
  attempt         INTEGER NOT NULL CHECK (attempt BETWEEN 1 AND 4),
  trigger         TEXT    NOT NULL CHECK (trigger IN ('auto','manual')),
  reason          TEXT    CHECK (reason IS NULL OR length(reason) BETWEEN 1 AND 500),
  note            TEXT    CHECK (note IS NULL OR length(note) BETWEEN 1 AND 4000),
  state           TEXT    NOT NULL CHECK (state IN ('running','succeeded','failed','cancelled')),
  base_sha        TEXT    CHECK (base_sha IS NULL OR length(base_sha) = 40),
  budget          TEXT    NOT NULL CHECK (json_valid(budget)),
  budget_warnings TEXT    NOT NULL DEFAULT '[]' CHECK (json_valid(budget_warnings)),
  resumed_lost    INTEGER NOT NULL DEFAULT 0 CHECK (resumed_lost IN (0,1)),
  rounds          INTEGER NOT NULL DEFAULT 0 CHECK (rounds >= 0),
  packet          TEXT    CHECK (packet IS NULL OR length(packet) <= 1048576),
  packet_sha256   TEXT    CHECK (packet_sha256 IS NULL OR length(packet_sha256) = 64),
  failure         TEXT    CHECK (failure IS NULL OR json_valid(failure)),
  started_at      INTEGER NOT NULL,
  ended_at        INTEGER,
  UNIQUE (spec_draft_id, attempt),
  CHECK ((state = 'running') = (ended_at IS NULL)),
  CHECK ((state = 'failed') = (failure IS NOT NULL)),
  CHECK ((trigger = 'manual') = (reason IS NOT NULL)),
  CHECK (trigger = 'manual' OR note IS NULL),
  CHECK ((packet IS NULL) = (packet_sha256 IS NULL)),
  CHECK (packet IS NULL OR base_sha IS NOT NULL)
) STRICT;
CREATE UNIQUE INDEX spec_draft_attempt_one_running ON spec_draft_attempt (spec_draft_id) WHERE state = 'running';

CREATE TABLE spec_draft_lint_round (
  attempt_id     TEXT    NOT NULL REFERENCES spec_draft_attempt(id),
  round          INTEGER NOT NULL CHECK (round BETWEEN 1 AND 10),
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
CREATE INDEX spec_draft_lint_round_draft_idx ON spec_draft_lint_round (spec_draft_id, created_at);

CREATE TABLE environment_v11 (
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
  CHECK (state <> 'ready' OR base_sha IS NOT NULL),
  CHECK ((work_item_id IS NOT NULL AND spec_draft_id IS NULL AND spec_draft_attempt_id IS NULL)
      OR (work_item_id IS NULL AND spec_draft_id IS NOT NULL AND spec_draft_attempt_id IS NOT NULL)),
  CHECK (work_item_id IS NULL OR branch IS NOT NULL),
  CHECK (spec_draft_id IS NULL OR (recipe_json IS NULL AND recipe_sha256 IS NULL
                                   AND port_start IS NULL AND port_count IS NULL))
) STRICT;

INSERT INTO environment_v11 (id, project_id, work_item_id, repo_id, root_path, worktree_path, branch, base_sha,
  state, created_at, updated_at, recipe_json, toolchains_json, port_start, port_count, recipe_sha256)
SELECT id, project_id, work_item_id, repo_id, root_path, worktree_path, branch, base_sha,
  state, created_at, updated_at, recipe_json, toolchains_json, port_start, port_count, recipe_sha256
FROM environment;

DROP TABLE environment;
ALTER TABLE environment_v11 RENAME TO environment;
CREATE UNIQUE INDEX environment_one_live ON environment (work_item_id) WHERE state IN ('preparing','ready');
CREATE UNIQUE INDEX environment_one_live_draft ON environment (spec_draft_id) WHERE state IN ('preparing','ready');
CREATE UNIQUE INDEX environment_port_live ON environment (port_start)
  WHERE port_start IS NOT NULL AND state IN ('preparing','ready','teardown_failed');

CREATE TABLE session_v11 (
  id               TEXT    PRIMARY KEY,
  project_id       TEXT    NOT NULL REFERENCES project(id),
  work_item_id     TEXT    REFERENCES work_item(id),
  stage_run_id     TEXT    REFERENCES stage_run(id),
  spec_draft_id    TEXT    REFERENCES spec_draft(id),
  spec_draft_attempt_id TEXT REFERENCES spec_draft_attempt(id),
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
  last_offset      INTEGER NOT NULL DEFAULT 0 CHECK (last_offset >= 0),
  turns            INTEGER NOT NULL DEFAULT 0,
  followups        INTEGER NOT NULL DEFAULT 0,
  running_ms       INTEGER NOT NULL DEFAULT 0,
  cost_usd_micros  INTEGER,
  input_tokens     INTEGER,
  output_tokens    INTEGER,
  end_reason       TEXT    CHECK (end_reason IS NULL OR json_valid(end_reason)),
  resumed_from_session_id TEXT REFERENCES session(id),
  resume_cause     TEXT    CHECK (resume_cause IS NULL OR resume_cause IN ('lost','retry')),
  idle_since       INTEGER,
  interrupt_requested TEXT CHECK (interrupt_requested IS NULL OR interrupt_requested IN ('operator','saka')),
  started_at       INTEGER,
  ended_at         INTEGER,
  created_at       INTEGER NOT NULL,
  updated_at       INTEGER NOT NULL,
  CHECK ((resumed_from_session_id IS NULL) = (resume_cause IS NULL)),
  CHECK ((state = 'idle') = (idle_since IS NOT NULL)),
  CHECK ((work_item_id IS NOT NULL AND stage_run_id IS NOT NULL
          AND spec_draft_id IS NULL AND spec_draft_attempt_id IS NULL)
      OR (work_item_id IS NULL AND stage_run_id IS NULL
          AND spec_draft_id IS NOT NULL AND spec_draft_attempt_id IS NOT NULL))
) STRICT;

INSERT INTO session_v11 (id, project_id, work_item_id, stage_run_id, environment_id, cli, cli_version,
  cli_session_id, model, account, state, runner_unit, socket_path, log_path, last_offset, turns, followups,
  running_ms, cost_usd_micros, input_tokens, output_tokens, end_reason, resumed_from_session_id, resume_cause,
  idle_since, interrupt_requested, started_at, ended_at, created_at, updated_at)
SELECT id, project_id, work_item_id, stage_run_id, environment_id, cli, cli_version,
  cli_session_id, model, account, state, runner_unit, socket_path, log_path, last_offset, turns, followups,
  running_ms, cost_usd_micros, input_tokens, output_tokens, end_reason, resumed_from_session_id, resume_cause,
  idle_since, interrupt_requested, started_at, ended_at, created_at, updated_at
FROM session;

DROP TABLE session;
ALTER TABLE session_v11 RENAME TO session;
CREATE INDEX session_work_item_idx ON session (work_item_id);
CREATE INDEX session_state_idx ON session (state);
CREATE UNIQUE INDEX session_one_live ON session (work_item_id)
  WHERE state IN ('starting','running','waiting','idle','interrupted');
CREATE INDEX session_stage_run_idx ON session (stage_run_id);
CREATE INDEX session_draft_attempt_idx ON session (spec_draft_attempt_id);
CREATE UNIQUE INDEX session_one_live_draft ON session (spec_draft_id)
  WHERE state IN ('starting','running','waiting','idle','interrupted');

CREATE TABLE permission_request_v11 (
  id             TEXT    PRIMARY KEY,
  project_id     TEXT    NOT NULL REFERENCES project(id),
  work_item_id   TEXT    REFERENCES work_item(id),
  spec_draft_id  TEXT    REFERENCES spec_draft(id),
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
  CHECK ((state = 'pending') = (answer_source IS NULL)),
  CHECK ((work_item_id IS NULL) <> (spec_draft_id IS NULL))
) STRICT;

INSERT INTO permission_request_v11 (id, project_id, work_item_id, session_id, cli_request_id, tool, input,
  summary, state, answer_source, message, requested_at, answered_at, delivered_at)
SELECT id, project_id, work_item_id, session_id, cli_request_id, tool, input,
  summary, state, answer_source, message, requested_at, answered_at, delivered_at
FROM permission_request;

DROP TABLE permission_request;
ALTER TABLE permission_request_v11 RENAME TO permission_request;
CREATE INDEX permission_request_pending_idx ON permission_request (state, requested_at);
CREATE INDEX permission_request_draft_idx ON permission_request (spec_draft_id);

CREATE TABLE session_activity_v11 (
  seq          INTEGER PRIMARY KEY,
  session_id   TEXT    NOT NULL REFERENCES session(id),
  "offset"     INTEGER NOT NULL CHECK ("offset" >= 0),
  work_item_id TEXT    REFERENCES work_item(id),
  spec_draft_id TEXT   REFERENCES spec_draft(id),
  project_id   TEXT    NOT NULL REFERENCES project(id),
  at           INTEGER NOT NULL,
  kind         TEXT    NOT NULL CHECK (kind IN ('system','thought','message','action','output','permission','result','steer')),
  summary      TEXT    NOT NULL CHECK (length(summary) <= 500),
  data         TEXT    NOT NULL CHECK (json_valid(data)),
  UNIQUE (session_id, "offset"),
  CHECK ((work_item_id IS NULL) <> (spec_draft_id IS NULL))
) STRICT;

INSERT INTO session_activity_v11 (seq, session_id, "offset", work_item_id, project_id, at, kind, summary, data)
SELECT seq, session_id, "offset", work_item_id, project_id, at, kind, summary, data
FROM session_activity;

DROP TABLE session_activity;
ALTER TABLE session_activity_v11 RENAME TO session_activity;

CREATE TABLE usage_record_v11 (
  id              TEXT    PRIMARY KEY,
  project_id      TEXT    NOT NULL REFERENCES project(id),
  work_item_id    TEXT    REFERENCES work_item(id),
  stage_run_id    TEXT    REFERENCES stage_run(id),
  spec_draft_id   TEXT    REFERENCES spec_draft(id),
  spec_draft_attempt_id TEXT REFERENCES spec_draft_attempt(id),
  session_id      TEXT    NOT NULL REFERENCES session(id),
  account         TEXT    NOT NULL,
  turn            INTEGER NOT NULL CHECK (turn >= 1),
  status          TEXT    NOT NULL,
  input_tokens    INTEGER CHECK (input_tokens IS NULL OR input_tokens >= 0),
  output_tokens   INTEGER CHECK (output_tokens IS NULL OR output_tokens >= 0),
  cost_usd_micros INTEGER CHECK (cost_usd_micros IS NULL OR cost_usd_micros >= 0),
  running_ms      INTEGER NOT NULL CHECK (running_ms >= 0),
  ended_at        INTEGER NOT NULL,
  UNIQUE (session_id, turn),
  CHECK ((work_item_id IS NOT NULL AND stage_run_id IS NOT NULL
          AND spec_draft_id IS NULL AND spec_draft_attempt_id IS NULL)
      OR (work_item_id IS NULL AND stage_run_id IS NULL
          AND spec_draft_id IS NOT NULL AND spec_draft_attempt_id IS NOT NULL))
) STRICT;

INSERT INTO usage_record_v11 (id, project_id, work_item_id, stage_run_id, session_id, account, turn, status,
  input_tokens, output_tokens, cost_usd_micros, running_ms, ended_at)
SELECT id, project_id, work_item_id, stage_run_id, session_id, account, turn, status,
  input_tokens, output_tokens, cost_usd_micros, running_ms, ended_at
FROM usage_record;

DROP TABLE usage_record;
ALTER TABLE usage_record_v11 RENAME TO usage_record;
CREATE INDEX usage_record_work_item_idx ON usage_record (work_item_id, ended_at);
CREATE INDEX usage_record_stage_run_idx ON usage_record (stage_run_id);
CREATE INDEX usage_record_account_idx ON usage_record (account, ended_at);
CREATE INDEX usage_record_draft_attempt_idx ON usage_record (spec_draft_attempt_id);
