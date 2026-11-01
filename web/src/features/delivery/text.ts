// Catalog lookups for server codes: unknown codes fall back to a generic message with
// the code, so a newer server never shows a blank.

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
  const values: Record<string, string | number> = {
    reason: text(params.reason) || "failed",
    checks: text(params.checks),
    minutes: typeof params.minutes === "number" ? params.minutes : 0,
  };
  return t(key, values);
}

export function errorText(t: T, code: string): string {
  const key = `error.${code}`;
  return known(key) ? t(key) : t("error.unknown", { code });
}
