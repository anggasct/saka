CREATE TABLE decision_request (
  id            TEXT PRIMARY KEY,
  project_id    TEXT NOT NULL REFERENCES project(id),
  session_id    TEXT NOT NULL REFERENCES session(id),
  owner         TEXT NOT NULL CHECK (json_valid(owner)),
  state         TEXT NOT NULL CHECK (state IN ('asked','answered','delivered','expired')),
  question      TEXT NOT NULL CHECK (length(question) BETWEEN 1 AND 500),
  context       TEXT CHECK (context IS NULL OR length(context) <= 2000),
  options       TEXT NOT NULL CHECK (json_valid(options)),
  answer_option INTEGER,
  answer_note   TEXT CHECK (answer_note IS NULL OR length(answer_note) <= 2000),
  answered_by   TEXT,
  asked_at      INTEGER NOT NULL,
  answered_at   INTEGER,
  delivered_at  INTEGER,
  expires_at    INTEGER NOT NULL
) STRICT;

CREATE UNIQUE INDEX decision_request_open ON decision_request(session_id) WHERE state = 'asked';
CREATE INDEX decision_request_project ON decision_request(project_id, state, asked_at DESC);
CREATE INDEX decision_request_work_item ON decision_request(json_extract(owner, '$.work_item_id'))
  WHERE json_extract(owner, '$.work_item_id') IS NOT NULL;
CREATE INDEX decision_request_draft ON decision_request(json_extract(owner, '$.spec_draft_id'))
  WHERE json_extract(owner, '$.spec_draft_id') IS NOT NULL;

CREATE TABLE blocker_v33 (
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
                 'docs_merge_failed','branch_moved','decision_expired')),
  params       TEXT    NOT NULL CHECK (json_valid(params)),
  state        TEXT    NOT NULL CHECK (state IN ('open','resolved')),
  resolution   TEXT    CHECK (resolution IS NULL OR resolution IN ('retried','cancelled','auto','taken_over')),
  opened_at    INTEGER NOT NULL,
  resolved_at  INTEGER,
  CHECK ((state = 'resolved') = (resolution IS NOT NULL AND resolved_at IS NOT NULL))
) STRICT;

INSERT INTO blocker_v33 (id, work_item_id, stage_run_id, project_id, code, params, state, resolution,
  opened_at, resolved_at)
SELECT id, work_item_id, stage_run_id, project_id, code, params, state, resolution, opened_at, resolved_at
FROM blocker;

DROP TABLE blocker;
ALTER TABLE blocker_v33 RENAME TO blocker;
CREATE UNIQUE INDEX blocker_one_open ON blocker (work_item_id) WHERE state = 'open';
