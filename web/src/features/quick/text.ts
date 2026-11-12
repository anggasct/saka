import { ApiError } from "@/api/client";
import { fieldParams } from "@/features/delivery/action-dialog";
import { errorText, record, text, type T } from "@/features/delivery/text";
import type { ProjectItem } from "@/features/delivery/queries";
import { fileErrorText } from "@/features/docs/text";
import { CATALOGS, type MessageKey } from "@/i18n/messages";
import { lintMessage } from "@/wasm/diagnostics";
import type { LintIssue, QuickFieldIssue } from "@/wasm/spec-tools";

function isKey(key: string): key is MessageKey {
  return key in CATALOGS.en;
}

const INDEXED = /^([a-z_]+)\[(\d+)\]$/;

export function fieldText(t: T, field: string): string {
  const indexed = INDEXED.exec(field);
  const name = indexed?.[1] ?? field;
  const key = `quick.field.${name}`;
  const label = isKey(key) ? t(key) : name;
  return indexed?.[2] === undefined
    ? label
    : t("quick.field.line", {
        field: label,
        line: Number(indexed[2]) + 1,
      });
}

export function issueText(t: T, issue: QuickFieldIssue): string {
  const key = `quick.error.${issue.code}`;
  return t(isKey(key) ? key : "quick.error.other", {
    field: fieldText(t, issue.field),
  });
}

export function lintLines(t: T, issues: readonly LintIssue[]): string[] {
  return issues
    .filter((issue) => issue.severity === "error")
    .map((issue) => lintMessage(issue, t));
}

function serverIssues(error: ApiError): QuickFieldIssue[] {
  const errors = error.params.errors;
  if (!Array.isArray(errors)) {
    return [];
  }
  return errors.flatMap((item: unknown) => {
    const entry = record(item);
    const field = text(entry.field);
    const code = text(entry.code);
    return field === "" || code === "" ? [] : [{ field, code }];
  });
}

function serverLint(error: ApiError): LintIssue[] {
  const issues = error.params.issues;
  if (!Array.isArray(issues)) {
    return [];
  }
  return issues.flatMap((item: unknown) => {
    const entry = record(item);
    const params = record(entry.params);
    return typeof entry.code === "string" &&
      (entry.severity === "error" || entry.severity === "warning") &&
      typeof entry.line === "number"
      ? [
          {
            code: entry.code,
            severity: entry.severity,
            line: entry.line,
            params,
          },
        ]
      : [];
  });
}

export function quickErrorLines(t: T, error: unknown): string[] {
  if (!(error instanceof ApiError)) {
    return [errorText(t, "network_error")];
  }
  if (error.code === "validation_failed") {
    const found = serverIssues(error);
    return found.length === 0
      ? [errorText(t, error.code, fieldParams(error.params))]
      : found.map((issue) => issueText(t, issue));
  }
  if (error.code === "spec_not_ready") {
    const lines = lintLines(t, serverLint(error));
    return [t("quick.error.notReady"), ...lines];
  }
  if (error.code === "quick_spec_not_approved") {
    const spec = record(error.params.spec);
    return [
      t("quick.error.notApproved", {
        path: text(spec.path),
        reason: text(error.params.code),
      }),
    ];
  }
  return [fileErrorText(t, error.code, error.params)];
}

const LEVELS = ["strict", "standard", "autonomous"] as const;

function rank(level: string | null | undefined): number {
  const found = LEVELS.findIndex((l) => l === level);
  return found === -1 ? LEVELS.length : found;
}

export function needsStartApproval(
  project: ProjectItem,
  kind: string,
  repo: string,
): boolean {
  const binding =
    project.repos.find((r) => (repo === "" ? r.is_default : r.name === repo)) ??
    project.repos[0];
  const caps = [
    binding?.autonomy.effective,
    kind === "security_patch" ? "strict" : null,
    project.workflow.kinds[kind]?.autonomy,
  ];
  return Math.min(...caps.map(rank)) === 0;
}
