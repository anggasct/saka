CREATE TABLE project_authorship (
  project_id           TEXT    PRIMARY KEY REFERENCES project(id),
  mode                 TEXT    NOT NULL CHECK (mode IN ('bot','user','user_pat')),
  mode_updated_at      INTEGER NOT NULL,
  token_nonce          BLOB    CHECK (token_nonce IS NULL OR length(token_nonce) = 24),
  token_ciphertext     BLOB,
  token_integration_id TEXT,
  token_user_id        INTEGER CHECK (token_user_id IS NULL OR token_user_id >= 1),
  token_login          TEXT    CHECK (token_login IS NULL OR length(token_login) BETWEEN 1 AND 39),
  token_name           TEXT,
  token_email          TEXT,
  token_expires_at     INTEGER,
  token_set_at         INTEGER,
  token_invalid_cause  TEXT    CHECK (token_invalid_cause IS NULL OR token_invalid_cause IN ('expired','rejected')),
  updated_at           INTEGER NOT NULL,
  CHECK ((token_ciphertext IS NULL) = (token_nonce IS NULL)),
  CHECK ((token_ciphertext IS NULL) = (token_set_at IS NULL)),
  CHECK ((token_ciphertext IS NULL) = (token_login IS NULL)),
  CHECK ((token_ciphertext IS NULL) = (token_integration_id IS NULL)),
  CHECK ((token_ciphertext IS NULL) = (token_email IS NULL)),
  CHECK (mode <> 'user_pat' OR token_ciphertext IS NOT NULL)
) STRICT, WITHOUT ROWID;

CREATE TABLE blocker_v15 (
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
                 'user_link_required','user_access_denied')),
  params       TEXT    NOT NULL CHECK (json_valid(params)),
  state        TEXT    NOT NULL CHECK (state IN ('open','resolved')),
  resolution   TEXT    CHECK (resolution IS NULL OR resolution IN ('retried','cancelled','auto')),
  opened_at    INTEGER NOT NULL,
  resolved_at  INTEGER,
  CHECK ((state = 'resolved') = (resolution IS NOT NULL AND resolved_at IS NOT NULL))
) STRICT;

INSERT INTO blocker_v15 (id, work_item_id, stage_run_id, project_id, code, params, state, resolution,
  opened_at, resolved_at)
SELECT id, work_item_id, stage_run_id, project_id, code, params, state, resolution, opened_at, resolved_at
FROM blocker;

DROP TABLE blocker;
ALTER TABLE blocker_v15 RENAME TO blocker;
CREATE UNIQUE INDEX blocker_one_open ON blocker (work_item_id) WHERE state = 'open';
