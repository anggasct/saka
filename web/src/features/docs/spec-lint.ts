import type { Diagnostic } from "@codemirror/lint";

import type { LintSource } from "@/editor/extensions";
import { saveDocument } from "@/editor/lossless";
import { toDiagnostics, type Translate } from "@/wasm/diagnostics";
import type { LintIssue, LintReport, SpecTools } from "@/wasm/spec-tools";

import type { Spec } from "./queries";

const CONTEXT_CODES = new Set([
  "reference_outside_workspace",
  "reference_not_doc",
  "reference_missing",
  "reference_not_text",
  "reference_too_large",
  "context_too_large",
]);

function isSeverity(value: unknown): value is LintIssue["severity"] {
  return value === "error" || value === "warning";
}

export function contextIssues(
  spec: Spec | null | undefined,
  hash: string | null,
): LintIssue[] {
  if (!spec || hash === null || hash !== spec.revision.content_hash) {
    return [];
  }
  return spec.lint.issues.flatMap((issue) => {
    if (!CONTEXT_CODES.has(issue.code)) {
      return [];
    }
    const params =
      typeof issue.params === "object" &&
      issue.params !== null &&
      !Array.isArray(issue.params)
        ? (issue.params as Record<string, unknown>)
        : {};
    return [
      {
        code: issue.code,
        severity: isSeverity(issue.severity) ? issue.severity : "error",
        line: issue.line,
        params,
      },
    ];
  });
}

export function specLintSource(
  tools: SpecTools,
  fileName: string,
  bom: boolean,
  extra: readonly LintIssue[],
  t: Translate,
): LintSource {
  return (state) => {
    const report = tools.lint(fileName, saveDocument(bom, state));
    if (!report.isSpec) {
      return [];
    }
    const issues = [...report.issues, ...extra];
    return toDiagnostics(state.doc, issues, t) satisfies Diagnostic[];
  };
}

export function issueCounts(issues: readonly LintIssue[]) {
  return {
    errors: issues.filter((i) => i.severity === "error").length,
    warnings: issues.filter((i) => i.severity === "warning").length,
  };
}

export type LocalLint = LintReport | null;
