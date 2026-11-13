ALTER TABLE deployment ADD COLUMN on_failure TEXT NOT NULL DEFAULT 'revert' CHECK (on_failure IN ('revert','fix'));

CREATE TABLE inbox_item_v44 (
  id            TEXT PRIMARY KEY,
  project_id    TEXT NOT NULL REFERENCES project(id),
  kind          TEXT NOT NULL CHECK (kind IN ('decision','approval','permission','blocker','spec_unclear','intake','deployment')),
  subject       TEXT NOT NULL CHECK (subject IN ('spec','step_start','merge','permission','blocker','decision','intake','deployment')),
  source_id     TEXT NOT NULL,
  work_item_id  TEXT REFERENCES work_item(id),
  spec_id       TEXT REFERENCES spec(id),
  state         TEXT NOT NULL CHECK (state IN ('open','answered','resolved','dismissed')),
  params        TEXT NOT NULL CHECK (json_valid(params)),
  action        TEXT NOT NULL CHECK (json_valid(action)),
  opened_at     INTEGER NOT NULL,
  answered_at   INTEGER,
  closed_at     INTEGER
) STRICT;

INSERT INTO inbox_item_v44 (id, project_id, kind, subject, source_id, work_item_id, spec_id, state, params, action, opened_at, answered_at, closed_at)
SELECT id, project_id, kind, subject, source_id, work_item_id, spec_id, state, params, action, opened_at, answered_at, closed_at FROM inbox_item;

DROP TABLE inbox_item;
ALTER TABLE inbox_item_v44 RENAME TO inbox_item;

CREATE UNIQUE INDEX inbox_item_live ON inbox_item(subject, source_id) WHERE state IN ('open','answered');
CREATE INDEX inbox_item_list ON inbox_item(state, opened_at DESC, id DESC);
CREATE INDEX inbox_item_project ON inbox_item(project_id, state);
CREATE INDEX inbox_item_work_item ON inbox_item(work_item_id) WHERE work_item_id IS NOT NULL;
CREATE INDEX inbox_item_spec ON inbox_item(spec_id) WHERE spec_id IS NOT NULL;

CREATE TABLE hook_run_v44 (
  id              TEXT    PRIMARY KEY,
  project_id      TEXT    NOT NULL REFERENCES project(id),
  hook            TEXT    NOT NULL CHECK (length(hook) BETWEEN 1 AND 32),
  action          TEXT    NOT NULL CHECK (action IN ('run','run_host','webhook','notify')),
  mode            TEXT    NOT NULL CHECK (mode IN ('after','gate')),
  trigger         TEXT    NOT NULL CHECK (trigger IN ('change_opened','review_done','merged','task_done','spec_shipped','blocked','inbox_item_opened','deployed','deploy_failed')),
  state           TEXT    NOT NULL CHECK (state IN ('pending','succeeded','failed')),
  work_item_id    TEXT    REFERENCES work_item(id),
  spec_id         TEXT    REFERENCES spec(id),
  repo_id         TEXT    REFERENCES repo(id),
  sha             TEXT    CHECK (sha IS NULL OR length(sha) = 40),
  context         TEXT    NOT NULL CHECK (json_valid(context)),
  source_event_id TEXT    NOT NULL,
  rerun_of        TEXT    REFERENCES hook_run(id),
  attempts        INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
  result          TEXT    CHECK (result IS NULL OR json_valid(result)),
  error           TEXT    CHECK (error IS NULL OR json_valid(error)),
  queued_at       INTEGER NOT NULL,
  finished_at     INTEGER,
  duration_ms     INTEGER CHECK (duration_ms IS NULL OR duration_ms >= 0),
  CHECK ((state = 'pending') = (finished_at IS NULL)),
  CHECK (state <> 'failed' OR error IS NOT NULL)
) STRICT;

INSERT INTO hook_run_v44 (id, project_id, hook, action, mode, trigger, state, work_item_id, spec_id, repo_id, sha, context, source_event_id, rerun_of, attempts, result, error, queued_at, finished_at, duration_ms)
SELECT id, project_id, hook, action, mode, trigger, state, work_item_id, spec_id, repo_id, sha, context, source_event_id, rerun_of, attempts, result, error, queued_at, finished_at, duration_ms FROM hook_run;

DROP TABLE hook_run;
ALTER TABLE hook_run_v44 RENAME TO hook_run;

CREATE UNIQUE INDEX hook_run_source_idx ON hook_run (project_id, hook, source_event_id) WHERE rerun_of IS NULL;
CREATE INDEX hook_run_project_idx ON hook_run (project_id, queued_at DESC, id DESC);
CREATE INDEX hook_run_work_item_idx ON hook_run (work_item_id, mode) WHERE work_item_id IS NOT NULL;
