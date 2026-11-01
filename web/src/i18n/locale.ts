import { platform } from "@/platform";

import { LOCALES } from "./messages";
import type { Locale } from "./messages";

/** The first preferred language Saka has a catalog for, else English. */
export function pickLocale(preferred: readonly string[]): Locale {
  for (const tag of preferred) {
    const base = tag.toLowerCase().split("-")[0];
    const found = LOCALES.find((l) => l === base);
    if (found !== undefined) {
      return found;
    }
  }
  return "en";
}

export function initialLocale(): Locale {
  return pickLocale(platform.preferredLocales());
}
