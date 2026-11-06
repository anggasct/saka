import type { MessageKey } from "@/i18n/messages";
import { CATALOGS } from "@/i18n/messages";

import type { WaitingOn, WorkItem } from "./queries";

export type T = (
  key: MessageKey,
  values?: Record<string, string | number>,
) => string;
export type Relative = (iso: string) => string;

const NONE = "none";

function known(key: string): key is MessageKey {
  return key in CATALOGS.en;
}

export function label(t: T, prefix: string, code: string): string {
  const key = `${prefix}.${code}`;
  return known(key) ? t(key) : code;
}

export function text(value: unknown): string {
  if (Array.isArray(value)) {
    return value.map(String).join(", ");
  }
  return typeof value === "string" || typeof value === "number"
    ? String(value)
    : "";
}

const MIB = 1024 * 1024;

export function sha(value: unknown): string {
  return text(value).slice(0, 12);
}

export function record(value: unknown): Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : {};
}

export function number(value: unknown): number {
  return typeof value === "number" ? value : 0;
}

const GATE_REASONS = [
  "no_checks",
  "required_missing",
  "worktree_moved",
  "runner_failed",
  "not_in_base",
  "docs_not_in_base",
];

function gateCase(code: string, params: Record<string, unknown>): string {
  const reason = text(params.reason);
  if (GATE_REASONS.includes(reason)) {
    return reason;
  }
  if (text(params.command) !== "") {
    return params.timed_out === true ? "command_timed_out" : "command_failed";
  }
  if (code === "dod_failed" && !Array.isArray(params.checks)) {
    return "not_in_base";
  }
  return "checks";
}

export function blockerText(
  t: T,
  code: string,
  params: Record<string, unknown>,
  relative?: Relative,
): string {
  const base = `blocker.${code}`;
  if (!known(base)) {
    return t("blocker.unknown", { code });
  }
  const docsKey = `${base}.docs`;
  const key = params.target === "docs" && known(docsKey) ? docsKey : base;
  const error = record(params.error);
  const errorParams = record(error.params);
  const field = text(params.field);
  const phase = text(params.phase);
  const resetsAt = text(params.resets_at);
  const remote = sha(params.remote_sha);
  const local = sha(params.local_sha);
  const values: Record<string, string | number> = {
    case: gateCase(code, params),
    reason: text(params.reason) || "failed",
    checks: text(params.checks),
    missing: text(params.missing),
    command: text(params.command),
    exit_code: text(params.exit_code),
    head: sha(params.head_sha),
    base: text(params.base_branch) || NONE,
    minutes: number(params.minutes),
    hours: number(params.hours),
    cause: text(params.cause) || "other",
    dimension: text(params.dimension) || "other",
    used: text(params.used),
    limit: text(params.limit),
    tool: text(params.tool),
    summary: text(params.summary),
    summary_known: text(params.summary) !== "" ? "yes" : "no",
    findings: number(params.findings),
    account: text(params.account),
    window: text(params.window),
    resets:
      resetsAt !== "" && relative !== undefined ? relative(resetsAt) : NONE,
    error: text(error.code) || "other",
    command_index: number(errorParams.command_index) + 1,
    docs_commit: sha(errorParams.commit),
    files: number(errorParams.files),
    max_files: number(errorParams.max_files),
    mib: Math.ceil(number(errorParams.bytes) / MIB),
    max_mib: Math.floor(number(errorParams.max_bytes) / MIB),
    timed_out: errorParams.timed_out === true ? "yes" : "no",
    issue: field ? `${text(params.code)} ${field}` : text(params.code),
    line: text(params.line) || NONE,
    source: text(params.source) || "other",
    value: text(params.value),
    version: text(params.version),
    name: text(params.name),
    names: Array.isArray(params.names) ? params.names.map(text).join(", ") : "",
    phase_known: phase !== "" ? "yes" : "no",
    phase: label(t, "phase", phase),
    dirty: params.dirty === true ? "yes" : "no",
    scope_stage: text(params.stage) || "other",
    base_branch: text(params.base_branch),
    paths: text(params.paths),
    count: number(params.count),
    more: Math.max(
      0,
      number(params.count) -
        (Array.isArray(params.paths) ? params.paths.length : 0),
    ),
    scope: text(params.scope),
    shas: remote !== "" && local !== "" ? "yes" : "no",
    remote,
    local,
    integration: text(params.integration_id),
    credential: text(params.credential) || "other",
    login: text(params.login),
    status: text(params.status),
    number: text(params.number),
    hook: text(params.hook),
    action: text(params.action) || "other",
    hook_exit: text(errorParams.exit_code),
    hook_status: text(errorParams.status),
    hook_detail: text(errorParams.name) || text(errorParams.channel),
  };
  return t(key, values);
}

export function errorText(
  t: T,
  code: string,
  params: Record<string, unknown> = {},
): string {
  const key = `error.${code}`;
  if (!known(key)) {
    return t("error.unknown", { code });
  }
  return t(key, {
    field: text(params.field),
    issue: text(params.code),
    head: sha(params.head_sha),
    outcome: label(t, "gate.outcome", text(params.gate_outcome)),
    stage: label(t, "stage", text(params.stage)),
    entry: label(t, "workflowEntry", text(params.entry)),
    number: text(params.number),
    limit: number(params.limit),
    reason: text(params.reason) || "other",
    integrations: Array.isArray(params.integrations)
      ? params.integrations
          .map((i) =>
            typeof i === "string" ? i : text(record(i).integration_id),
          )
          .join(", ")
      : "",
    projects: Array.isArray(params.projects)
      ? params.projects.map((p) => text(record(p).name)).join(", ")
      : "",
    cause: text(params.cause) || "other",
    status: text(params.status),
    hook: text(params.hook),
    repos: Array.isArray(params.repos)
      ? params.repos.map((r) => text(record(r).full_name)).join(", ")
      : "",
  });
}

export function waitingText(
  t: T,
  item: WaitingOn,
  relative: Relative,
  percent: (value: number) => string,
): string {
  switch (item.kind) {
    case "step":
      return t("waiting.step", { step: item.step_id });
    case "spec":
      return t("waiting.spec", { slug: item.slug });
    case "quota":
      return t("waiting.quota", {
        account: item.account,
        window: item.window,
        percent: percent(item.utilization),
        resets:
          item.resets_at !== null && item.resets_at !== undefined
            ? relative(item.resets_at)
            : NONE,
      });
    case "port_block":
      return t("waiting.port_block");
    case "architect_slot":
      return t("waiting.architect_slot");
  }
}

export function holdsCount(task: WorkItem, tasks: readonly WorkItem[]): number {
  if (task.state === "done") {
    return 0;
  }
  return tasks.filter(
    (other) =>
      other.id !== task.id &&
      other.waiting_on.some(
        (w) =>
          (w.kind === "step" && w.work_item_id === task.id) ||
          (w.kind === "spec" &&
            task.spec_id !== null &&
            task.spec_id !== undefined &&
            w.spec_id === task.spec_id),
      ),
  ).length;
}
