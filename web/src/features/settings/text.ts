import { errorText, label, record, text } from "@/features/delivery/text";
import type { T } from "@/features/delivery/text";
import { CATALOGS } from "@/i18n/messages";
import type { MessageKey } from "@/i18n/messages";

const NONE = "none";

function known(key: string): key is MessageKey {
  return key in CATALOGS.en;
}

export type Issue = { code: string; field: string; line: string };

export function issuesOf(params: Record<string, unknown>): Issue[] {
  const issues = params.issues;
  if (!Array.isArray(issues)) {
    return [];
  }
  return issues.map((raw) => {
    const issue = record(raw);
    return {
      code: text(issue.code),
      field: text(issue.field),
      line: text(issue.line),
    };
  });
}

export function issueText(t: T, issue: Issue): string {
  const key = `projectConfigIssue.${issue.code}`;
  const sentence = known(key)
    ? t(key)
    : t("projectConfigIssue.unknown", { code: issue.code });
  return t("settings.config.issue", {
    line: issue.line === "" ? NONE : issue.line,
    field: issue.field === "" ? NONE : issue.field,
    sentence,
  });
}

export type BlockedRepo = {
  name: string;
  tasks: { id: string; key: string; state: string }[];
};

export function blockedRepos(params: Record<string, unknown>): BlockedRepo[] {
  const repos = params.repos;
  if (!Array.isArray(repos)) {
    return [];
  }
  return repos.map((raw) => {
    const repo = record(raw);
    const tasks = Array.isArray(repo.tasks) ? repo.tasks : [];
    return {
      name: text(repo.name),
      tasks: tasks.map((task) => {
        const item = record(task);
        return {
          id: text(item.work_item_id),
          key: text(item.task_key),
          state: text(item.state),
        };
      }),
    };
  });
}

function problemValues(params: Record<string, unknown>) {
  const repo = text(params.repo);
  return {
    host: text(params.host),
    repo: repo === "" ? NONE : repo,
    repos: blockedRepos(params)
      .map((r) => r.name)
      .join(", "),
  };
}

export function configErrorText(
  t: T,
  code: string,
  params: Record<string, unknown>,
): string {
  const key = `configError.${code}`;
  if (!known(key)) {
    return t("configError.unknown", { code });
  }
  return t(key, problemValues(params));
}

function detailed(t: T, params: Record<string, unknown>) {
  const integrations = params.integrations;
  const repos = params.repos;
  return {
    ...params,
    integrations: Array.isArray(integrations)
      ? integrations.map((raw) => {
          if (typeof raw === "string") {
            return raw;
          }
          const item = record(raw);
          const state = text(item.state);
          const id = text(item.integration_id);
          return state === ""
            ? id
            : `${id} (${label(t, "settings.linkState", state)})`;
        })
      : integrations,
    repos: Array.isArray(repos)
      ? repos.map((raw) => {
          const item = record(raw);
          const status = text(item.status);
          const name = text(item.full_name);
          return { full_name: status === "" ? name : `${name} (${status})` };
        })
      : repos,
  };
}

export function settingsErrorText(
  t: T,
  code: string,
  params: Record<string, unknown> = {},
): string {
  return errorText(t, code, detailed(t, params));
}

export function warningText(
  t: T,
  code: string,
  params: Record<string, unknown>,
): string {
  if (code === "path_scope_overlap") {
    return t("settings.warning.path_scope_overlap", {
      repo: text(params.repo),
      other_project: text(params.other_project),
      other_repo: text(params.other_repo),
    });
  }
  return t("settings.warning.unknown", { code });
}

export function configPath(projectPath: string): string {
  return projectPath === "." || projectPath === ""
    ? "saka.toml"
    : `${projectPath}/saka.toml`;
}

export function shortSha(hash: string): string {
  return hash.replace(/^sha256:/, "").slice(0, 12);
}

const SOON_DAYS = 7;
const DAY_MS = 24 * 60 * 60 * 1000;

export function expiresSoon(iso: string, now: number): boolean {
  return Date.parse(iso) - now < SOON_DAYS * DAY_MS;
}
