CREATE TABLE hook_run (
  id              TEXT    PRIMARY KEY,
  project_id      TEXT    NOT NULL REFERENCES project(id),
  hook            TEXT    NOT NULL CHECK (length(hook) BETWEEN 1 AND 32),
  action          TEXT    NOT NULL CHECK (action IN ('run','run_host','webhook','notify')),
  mode            TEXT    NOT NULL CHECK (mode IN ('after','gate')),
  trigger         TEXT    NOT NULL CHECK (trigger IN ('change_opened','review_done','merged','task_done','spec_shipped','blocked')),
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

CREATE UNIQUE INDEX hook_run_source_idx ON hook_run (project_id, hook, source_event_id) WHERE rerun_of IS NULL;
CREATE INDEX hook_run_project_idx ON hook_run (project_id, queued_at DESC, id DESC);
CREATE INDEX hook_run_work_item_idx ON hook_run (work_item_id, mode) WHERE work_item_id IS NOT NULL;
