CREATE TABLE session_v5 (
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
  CHECK ((state = 'idle') = (idle_since IS NOT NULL))
) STRICT;

INSERT INTO session_v5 (id, project_id, work_item_id, stage_run_id, environment_id, cli, cli_version, cli_session_id,
  model, account, state, runner_unit, socket_path, log_path, last_offset, turns, followups, running_ms,
  cost_usd_micros, input_tokens, output_tokens, end_reason, idle_since, started_at, ended_at, created_at, updated_at)
SELECT id, project_id, work_item_id, stage_run_id, environment_id, cli, cli_version, cli_session_id,
  model, account, state, runner_unit, socket_path, log_path, last_offset, turns, followups, running_ms,
  cost_usd_micros, input_tokens, output_tokens, end_reason, CASE WHEN state = 'idle' THEN updated_at END,
  started_at, ended_at, created_at, updated_at
FROM session;

DROP TABLE session;
ALTER TABLE session_v5 RENAME TO session;
CREATE INDEX session_work_item_idx ON session (work_item_id);
CREATE INDEX session_state_idx ON session (state);
CREATE UNIQUE INDEX session_one_live ON session (work_item_id)
  WHERE state IN ('starting','running','waiting','idle','interrupted');
CREATE INDEX session_stage_run_idx ON session (stage_run_id);

ALTER TABLE stage_run ADD COLUMN budget TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(budget));
ALTER TABLE stage_run ADD COLUMN budget_warnings TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(budget_warnings));

CREATE TABLE session_activity (
  seq          INTEGER PRIMARY KEY,
  session_id   TEXT    NOT NULL REFERENCES session(id),
  "offset"     INTEGER NOT NULL CHECK ("offset" >= 0),
  work_item_id TEXT    NOT NULL REFERENCES work_item(id),
  project_id   TEXT    NOT NULL REFERENCES project(id),
  at           INTEGER NOT NULL,
  kind         TEXT    NOT NULL CHECK (kind IN ('system','thought','message','action','output','permission','result','steer')),
  summary      TEXT    NOT NULL CHECK (length(summary) <= 500),
  data         TEXT    NOT NULL CHECK (json_valid(data)),
  UNIQUE (session_id, "offset")
) STRICT;

CREATE TABLE permission_rule (
  id                    TEXT    PRIMARY KEY,
  project_id            TEXT    NOT NULL REFERENCES project(id),
  effect                TEXT    NOT NULL CHECK (effect IN ('allow')),
  tool                  TEXT    NOT NULL,
  command               TEXT,
  permission_request_id TEXT    NOT NULL REFERENCES permission_request(id),
  created_by            TEXT    NOT NULL,
  created_at            INTEGER NOT NULL,
  revoked_at            INTEGER,
  revoke_reason         TEXT    CHECK (revoke_reason IS NULL OR length(revoke_reason) BETWEEN 1 AND 500),
  CHECK (tool <> 'Bash' OR command IS NOT NULL),
  CHECK ((revoked_at IS NULL) = (revoke_reason IS NULL))
) STRICT;
CREATE UNIQUE INDEX permission_rule_active ON permission_rule (project_id, tool, ifnull(command, ''))
  WHERE revoked_at IS NULL;

CREATE TABLE usage_record (
  id              TEXT    PRIMARY KEY,
  project_id      TEXT    NOT NULL REFERENCES project(id),
  work_item_id    TEXT    NOT NULL REFERENCES work_item(id),
  stage_run_id    TEXT    NOT NULL REFERENCES stage_run(id),
  session_id      TEXT    NOT NULL REFERENCES session(id),
  account         TEXT    NOT NULL,
  turn            INTEGER NOT NULL CHECK (turn >= 1),
  status          TEXT    NOT NULL,
  input_tokens    INTEGER CHECK (input_tokens IS NULL OR input_tokens >= 0),
  output_tokens   INTEGER CHECK (output_tokens IS NULL OR output_tokens >= 0),
  cost_usd_micros INTEGER CHECK (cost_usd_micros IS NULL OR cost_usd_micros >= 0),
  running_ms      INTEGER NOT NULL CHECK (running_ms >= 0),
  ended_at        INTEGER NOT NULL,
  UNIQUE (session_id, turn)
) STRICT;
CREATE INDEX usage_record_work_item_idx ON usage_record (work_item_id, ended_at);
CREATE INDEX usage_record_stage_run_idx ON usage_record (stage_run_id);
CREATE INDEX usage_record_account_idx ON usage_record (account, ended_at);

CREATE TABLE account_quota (
  account     TEXT    NOT NULL,
  window      TEXT    NOT NULL,
  utilization REAL    NOT NULL CHECK (utilization >= 0),
  status      TEXT    NOT NULL,
  resets_at   INTEGER,
  observed_at INTEGER NOT NULL,
  PRIMARY KEY (account, window)
) STRICT, WITHOUT ROWID;
