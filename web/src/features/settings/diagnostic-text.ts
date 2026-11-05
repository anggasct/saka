import { record, text } from "@/features/delivery/text";
import type { Relative, T } from "@/features/delivery/text";
import { CATALOGS } from "@/i18n/messages";
import type { MessageKey } from "@/i18n/messages";

const FRACTION = 1;
const PERCENT = 100;

function known(key: string): key is MessageKey {
  return key in CATALOGS.en;
}

function item(value: unknown): string {
  if (typeof value !== "object" || value === null) {
    return text(value);
  }
  const fields = record(value);
  if (typeof fields.tool === "string") {
    return [fields.tool, fields.version ?? fields.pinned]
      .filter((part) => part !== undefined && part !== null)
      .map(text)
      .join(" ");
  }
  if (typeof fields.permission === "string") {
    return fields.permission;
  }
  if (typeof fields.window === "string") {
    return fields.window;
  }
  if (typeof fields.name === "string") {
    return fields.name;
  }
  return Object.values(fields).map(text).join(" ");
}

function value(key: string, raw: unknown, relative: Relative): string {
  if (Array.isArray(raw)) {
    return raw.map(item).join(", ");
  }
  if (key.endsWith("_at") && typeof raw === "string") {
    return relative(raw);
  }
  if (key === "utilization" && typeof raw === "number") {
    const share = raw <= FRACTION ? raw * PERCENT : raw;
    return `${Math.round(share)}%`;
  }
  return item(raw);
}

export function paramValues(
  params: Record<string, unknown>,
  relative: Relative,
): Record<string, string> {
  return Object.fromEntries(
    Object.entries(params).map(([key, raw]) => [
      key,
      value(key, raw, relative),
    ]),
  );
}

function filled(
  key: MessageKey,
  params: Record<string, unknown>,
  relative: Relative,
): Record<string, string> {
  const blanks: Record<string, string> = {};
  for (const match of CATALOGS.en[key].matchAll(/\{(\w+)/g)) {
    blanks[match[1] ?? ""] = "";
  }
  return { ...blanks, ...paramValues(params, relative) };
}

export function diagnosticText(
  t: T,
  relative: Relative,
  code: string,
  params: Record<string, unknown>,
): string {
  const key = `diagnostic.${code}`;
  return known(key)
    ? t(key, filled(key, params, relative))
    : t("diagnostic.unknown", { code });
}

export function fixText(
  t: T,
  relative: Relative,
  code: string,
  params: Record<string, unknown>,
): string | null {
  const key = `diagnosticFix.${code}`;
  return known(key) ? t(key, filled(key, params, relative)) : null;
}
