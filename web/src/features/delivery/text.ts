import type { MessageKey } from "@/i18n/messages";
import { CATALOGS } from "@/i18n/messages";

type T = (key: MessageKey, values?: Record<string, string | number>) => string;

function known(key: string): key is MessageKey {
  return key in CATALOGS.en;
}

export function label(t: T, prefix: string, code: string): string {
  const key = `${prefix}.${code}`;
  return known(key) ? t(key) : code;
}

function text(value: unknown): string {
  if (Array.isArray(value)) {
    return value.map(String).join(", ");
  }
  return typeof value === "string" || typeof value === "number"
    ? String(value)
    : "";
}

export function blockerText(
  t: T,
  code: string,
  params: Record<string, unknown>,
): string {
  const key = `blocker.${code}`;
  if (!known(key)) {
    return t("blocker.unknown", { code });
  }
  const number = (value: unknown) => (typeof value === "number" ? value : 0);
  const record = (value: unknown): Record<string, unknown> =>
    typeof value === "object" && value !== null && !Array.isArray(value)
      ? (value as Record<string, unknown>)
      : {};
  const error = record(params.error);
  const errorParams = record(error.params);
  const field = text(params.field);
  const values: Record<string, string | number> = {
    reason: text(params.reason) || "failed",
    checks: text(params.checks),
    minutes: number(params.minutes),
    hours: number(params.hours),
    cause: text(params.cause) || "other",
    dimension: text(params.dimension) || "other",
    used: text(params.used),
    limit: text(params.limit),
    tool: text(params.tool),
    summary: text(params.summary),
    account: text(params.account),
    window: text(params.window),
    error: text(error.code) || "other",
    command: number(errorParams.command_index) + 1,
    timed_out: errorParams.timed_out === true ? "yes" : "no",
    issue: field ? `${text(params.code)} ${field}` : text(params.code),
    line: text(params.line) || "none",
  };
  return t(key, values);
}

export function errorText(t: T, code: string): string {
  const key = `error.${code}`;
  return known(key) ? t(key) : t("error.unknown", { code });
}
