CREATE TABLE permission_rule_v32 (
  id                    TEXT    PRIMARY KEY,
  project_id            TEXT    NOT NULL REFERENCES project(id),
  effect                TEXT    NOT NULL CHECK (effect IN ('allow','deny','ask')),
  tool                  TEXT    NOT NULL,
  match_kind            TEXT    NOT NULL CHECK (match_kind IN ('exact','glob')),
  command               TEXT    CHECK (command IS NULL OR length(command) <= 2000),
  path                  TEXT,
  note                  TEXT    CHECK (note IS NULL OR length(note) <= 500),
  permission_request_id TEXT    REFERENCES permission_request(id),
  created_by            TEXT    NOT NULL,
  created_at            INTEGER NOT NULL,
  revoked_at            INTEGER,
  revoke_reason         TEXT    CHECK (revoke_reason IS NULL OR length(revoke_reason) BETWEEN 1 AND 500),
  CHECK (tool <> 'Bash' OR command IS NOT NULL),
  CHECK ((revoked_at IS NULL) = (revoke_reason IS NULL))
) STRICT;

INSERT INTO permission_rule_v32 (id, project_id, effect, tool, match_kind, command, path, note,
  permission_request_id, created_by, created_at, revoked_at, revoke_reason)
SELECT id, project_id, effect, tool, 'exact', command, NULL, NULL, permission_request_id, created_by, created_at,
  revoked_at, revoke_reason
FROM permission_rule;

DROP TABLE permission_rule;
ALTER TABLE permission_rule_v32 RENAME TO permission_rule;
CREATE UNIQUE INDEX permission_rule_active
  ON permission_rule (project_id, effect, tool, match_kind, ifnull(command, ''), ifnull(path, ''))
  WHERE revoked_at IS NULL;
