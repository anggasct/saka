// Message catalogs. `en` defines the keys; every locale has the same keys.

import en from "./en.json";
import id from "./id.json";

export type MessageKey = keyof typeof en;

export const LOCALES = ["en", "id"] as const;
export type Locale = (typeof LOCALES)[number];

export const CATALOGS: Record<Locale, Record<MessageKey, string>> = { en, id };

/** Keys present in one catalog and missing in another, per locale. */
export function missingKeys(
  catalogs: Record<string, Record<string, string>>,
): Record<string, string[]> {
  const all = new Set(Object.values(catalogs).flatMap((c) => Object.keys(c)));
  const missing: Record<string, string[]> = {};
  for (const [locale, catalog] of Object.entries(catalogs)) {
    const gaps = [...all].filter((key) => !(key in catalog)).sort();
    if (gaps.length > 0) {
      missing[locale] = gaps;
    }
  }
  return missing;
}
