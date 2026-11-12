export type LintSeverity = "error" | "warning";

export type LintIssue = {
  code: string;
  severity: LintSeverity;
  line: number;
  params: Record<string, unknown>;
};

export type StepLine = {
  id: string;
  line: number;
};

export type LintReport = {
  isSpec: boolean;
  parserVersion: number;
  issues: LintIssue[];
  steps: StepLine[];
};

export type LintCodeInfo = {
  code: string;
  severity: LintSeverity;
};

export type QuickInput = {
  kind: string;
  title: string;
  description: string;
  done_when: readonly string[];
  regression: readonly string[];
  ac: readonly string[];
  locale: string;
  repo: string;
};

export type QuickFieldIssue = {
  field: string;
  code: string;
};

export type QuickPreview = {
  valid: boolean;
  issues: QuickFieldIssue[];
  structure: boolean;
  text: string;
  slug: string | null;
  lint: LintIssue[];
};

export type SpecTools = {
  lint(fileName: string, text: string): LintReport;
  quickPreview(input: QuickInput): QuickPreview;
  contentHash(text: string): string;
  lintCodes(): LintCodeInfo[];
};

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function isSeverity(value: unknown): value is LintSeverity {
  return value === "error" || value === "warning";
}

function isLine(value: unknown): value is number {
  return typeof value === "number" && Number.isInteger(value) && value >= 1;
}

function isIssue(value: unknown): value is LintIssue {
  return (
    isRecord(value) &&
    typeof value.code === "string" &&
    isSeverity(value.severity) &&
    isLine(value.line) &&
    isRecord(value.params)
  );
}

function isStep(value: unknown): value is StepLine {
  return isRecord(value) && typeof value.id === "string" && isLine(value.line);
}

function isCodeInfo(value: unknown): value is LintCodeInfo {
  return (
    isRecord(value) &&
    typeof value.code === "string" &&
    isSeverity(value.severity)
  );
}

function isFieldIssue(value: unknown): value is QuickFieldIssue {
  return (
    isRecord(value) &&
    typeof value.field === "string" &&
    typeof value.code === "string"
  );
}

function invalid(what: string): never {
  throw new Error(`saka-wasm returned an invalid ${what}`);
}

export function parseLintReport(json: string): LintReport {
  const value: unknown = JSON.parse(json);
  if (
    !isRecord(value) ||
    typeof value.is_spec !== "boolean" ||
    typeof value.parser_version !== "number" ||
    !Array.isArray(value.issues) ||
    !value.issues.every(isIssue) ||
    !Array.isArray(value.steps) ||
    !value.steps.every(isStep)
  ) {
    return invalid("lint report");
  }
  return {
    isSpec: value.is_spec,
    parserVersion: value.parser_version,
    issues: value.issues,
    steps: value.steps,
  };
}

export function parseQuickPreview(json: string): QuickPreview {
  const value: unknown = JSON.parse(json);
  if (
    !isRecord(value) ||
    typeof value.valid !== "boolean" ||
    !Array.isArray(value.issues) ||
    !value.issues.every(isFieldIssue) ||
    typeof value.text !== "string" ||
    !Array.isArray(value.lint) ||
    !value.lint.every(isIssue)
  ) {
    return invalid("quick task preview");
  }
  return {
    valid: value.valid,
    issues: value.issues,
    structure: value.structure !== false,
    text: value.text,
    slug: typeof value.slug === "string" ? value.slug : null,
    lint: value.lint,
  };
}

export function parseLintCodes(json: string): LintCodeInfo[] {
  const value: unknown = JSON.parse(json);
  if (!Array.isArray(value) || !value.every(isCodeInfo)) {
    return invalid("lint code list");
  }
  return value;
}

export async function loadSpecTools(): Promise<SpecTools> {
  const wasm = await import("./pkg/saka_wasm");
  await wasm.default();
  return {
    lint: (fileName, text) => parseLintReport(wasm.lint(fileName, text)),
    quickPreview: (input) =>
      parseQuickPreview(wasm.quick_preview(JSON.stringify(input))),
    contentHash: (text) => wasm.content_hash(text),
    lintCodes: () => parseLintCodes(wasm.lint_codes()),
  };
}
