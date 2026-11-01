CREATE TABLE environment_v4 (
  id            TEXT    PRIMARY KEY,
  project_id    TEXT    NOT NULL REFERENCES project(id),
  work_item_id  TEXT    NOT NULL REFERENCES work_item(id),
  repo_id       TEXT    NOT NULL REFERENCES repo(id),
  root_path     TEXT    NOT NULL,
  worktree_path TEXT    NOT NULL,
  branch        TEXT    NOT NULL,
  base_sha      TEXT    CHECK (base_sha IS NULL OR length(base_sha) = 40),
  state         TEXT    NOT NULL CHECK (state IN ('preparing','ready','failed','teardown_failed','torn_down')),
  created_at    INTEGER NOT NULL,
  updated_at    INTEGER NOT NULL,
  CHECK (state <> 'ready' OR base_sha IS NOT NULL)
) STRICT;

INSERT INTO environment_v4 (id, project_id, work_item_id, repo_id, root_path, worktree_path, branch, base_sha,
  state, created_at, updated_at)
SELECT id, project_id, work_item_id, repo_id, root_path, worktree_path, branch, base_sha,
  state, created_at, updated_at
FROM environment;

DROP TABLE environment;
ALTER TABLE environment_v4 RENAME TO environment;
CREATE UNIQUE INDEX environment_one_live ON environment (work_item_id) WHERE state IN ('preparing','ready');
