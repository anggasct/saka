import { CATALOGS, type MessageKey } from "@/i18n/messages";

import type { LintIssue, LintSeverity } from "./spec-tools";

export type LineSpan = {
  from: number;
  to: number;
};

export type LinedText = {
  readonly lines: number;
  line(n: number): LineSpan;
};

export type SpecDiagnostic = {
  from: number;
  to: number;
  severity: LintSeverity;
  message: string;
  source: "saka";
};

export type Translate = (
  key: MessageKey,
  values?: Record<string, string | number>,
) => string;

function isMessageKey(key: string): key is MessageKey {
  return key in CATALOGS.en;
}

function formatValue(value: unknown): string | number {
  if (typeof value === "string" || typeof value === "number") {
    return value;
  }
  if (Array.isArray(value)) {
    return value.map(formatValue).join(", ");
  }
  return JSON.stringify(value);
}

export function lintMessage(issue: LintIssue, t: Translate): string {
  const key = `lint.${issue.code}`;
  if (!isMessageKey(key)) {
    return t("lint.unknown", { code: issue.code });
  }
  const values: Record<string, string | number> = {};
  for (const [name, value] of Object.entries(issue.params)) {
    values[name] = formatValue(value);
  }
  return t(key, values);
}

export function toDiagnostics(
  doc: LinedText,
  issues: readonly LintIssue[],
  t: Translate,
): SpecDiagnostic[] {
  return issues.map((issue) => {
    const line = doc.line(Math.min(Math.max(issue.line, 1), doc.lines));
    return {
      from: line.from,
      to: line.to,
      severity: issue.severity,
      message: lintMessage(issue, t),
      source: "saka",
    };
  });
}
