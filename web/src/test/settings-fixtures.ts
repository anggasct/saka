import type { Project } from "@/features/settings/queries";

import { project, PROJECT_ID } from "./fixtures";

export const PROJECTS = "/api/v1/projects";
export const PROJECT = `/api/v1/projects/${PROJECT_ID}`;
export const SECRETS = `${PROJECT}/secrets`;
export const RELOAD = `${PROJECT}/actions/reload`;

export const OTHER_PROJECT_ID = "0193f0c4-1b2a-7c3d-9e8f-001122334499";

const REPO_ID = project.workspace.repo_id;

export const twoRepos: Project = {
  ...project,
  repos: [
    {
      name: "docs",
      repo_id: REPO_ID,
      integration_id: "github",
      provider: "github",
      full_name: "acme/tasks",
      url: "https://github.com/acme/tasks",
      default_branch: "main",
      base_branch: "main",
      is_default: true,
      is_workspace: true,
      path_scope: ["services/**"],
      effective_path_scope: ["services/**", "docs/**"],
      autonomy: {
        requested: "strict",
        allowed: "standard",
        effective: "strict",
      },
    },
    {
      name: "web",
      repo_id: "0193f0c4-1b2a-7c3d-9e8f-001122334401",
      integration_id: "github",
      provider: "github",
      full_name: "acme/web",
      url: "https://github.com/acme/web",
      default_branch: "main",
      base_branch: "develop",
      is_default: false,
      is_workspace: false,
      path_scope: ["**"],
      effective_path_scope: ["**"],
      autonomy: {
        requested: "autonomous",
        allowed: "standard",
        effective: "standard",
      },
    },
  ],
  warnings: [
    {
      code: "path_scope_overlap",
      params: {
        repo: "web",
        repo_id: "0193f0c4-1b2a-7c3d-9e8f-001122334401",
        other_project_id: OTHER_PROJECT_ID,
        other_project: "storefront",
        other_repo: "shop",
      },
    },
  ],
};

const OBSERVED = "2026-10-31T10:00:00.000Z";
const COMMIT = "a1b2c3d4e5f6".padEnd(40, "0");

export const invalidConfig: Project = {
  ...project,
  config_error: {
    code: "project_config_invalid",
    commit: COMMIT,
    observed_at: OBSERVED,
    params: {
      issues: [
        { code: "field_missing", field: "repo.url", line: 4 },
        { code: "toml_invalid", field: null, line: 9 },
      ],
    },
  },
};

export const repoInUse: Project = {
  ...project,
  config_error: {
    code: "project_repo_in_use",
    commit: COMMIT,
    observed_at: OBSERVED,
    params: {
      repos: [
        {
          name: "web",
          repo_id: "0193f0c4-1b2a-7c3d-9e8f-001122334401",
          tasks: [
            {
              work_item_id: "0193f0c4-1b2a-7c3d-9e8f-001122334501",
              task_key: "tasks-done/s1",
              state: "in_stage",
            },
          ],
        },
      ],
    },
  },
};

export const SECRET_ITEMS = [
  { name: "API_KEY", updated_at: "2026-10-30T10:00:00.000Z" },
  { name: "NPM_TOKEN", updated_at: "2026-10-31T09:00:00.000Z" },
];

export function problemBody(status: number, code: string, params: object = {}) {
  return {
    status,
    body: { type: "", title: code, status, code, params, correlation_id: null },
  };
}
