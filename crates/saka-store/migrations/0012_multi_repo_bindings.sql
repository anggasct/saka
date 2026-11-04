-- A project binds 1..=16 code repos, each with its own base branch; saka.toml is reloaded by hash.

ALTER TABLE project ADD COLUMN config_sha256 TEXT NOT NULL
  DEFAULT 'sha256:0000000000000000000000000000000000000000000000000000000000000000'
  CHECK (length(config_sha256) = 71);
ALTER TABLE project ADD COLUMN config_error TEXT CHECK (config_error IS NULL OR json_valid(config_error));

CREATE TABLE project_repo_v2 (
  project_id  TEXT    NOT NULL REFERENCES project(id),
  repo_id     TEXT    NOT NULL REFERENCES repo(id),
  name        TEXT    NOT NULL CHECK (length(name) BETWEEN 1 AND 32),
  is_default  INTEGER NOT NULL CHECK (is_default IN (0,1)),
  base_branch TEXT    NOT NULL CHECK (length(base_branch) BETWEEN 1 AND 255),
  path_scope  TEXT    NOT NULL CHECK (json_valid(path_scope) AND json_type(path_scope) = 'array'
                                      AND json_array_length(path_scope) BETWEEN 1 AND 64),
  created_at  INTEGER NOT NULL,
  updated_at  INTEGER NOT NULL,
  PRIMARY KEY (project_id, repo_id),
  UNIQUE (project_id, name)
) STRICT;

INSERT INTO project_repo_v2 (project_id, repo_id, name, is_default, base_branch, path_scope, created_at, updated_at)
SELECT pr.project_id, pr.repo_id, pr.name, pr.is_default, p.base_branch, pr.path_scope, pr.created_at, pr.updated_at
FROM project_repo pr JOIN project p ON p.id = pr.project_id;

DROP TABLE project_repo;
ALTER TABLE project_repo_v2 RENAME TO project_repo;
CREATE UNIQUE INDEX project_repo_one_default ON project_repo (project_id) WHERE is_default = 1;
CREATE INDEX project_repo_repo_idx ON project_repo (repo_id);
