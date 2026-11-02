import { platform } from "@/platform";

import { LOCALES } from "./messages";
import type { Locale } from "./messages";

const PREFERENCE = "locale";

function known(value: string | null): Locale | undefined {
  return LOCALES.find((l) => l === value);
}

export function pickLocale(preferred: readonly string[]): Locale {
  for (const tag of preferred) {
    const found = known(tag.toLowerCase().split("-")[0] ?? null);
    if (found !== undefined) {
      return found;
    }
  }
  return "en";
}

export function initialLocale(): Locale {
  return (
    known(platform.preferences.get(PREFERENCE)) ??
    pickLocale(platform.preferredLocales())
  );
}

export function rememberLocale(locale: Locale): void {
  platform.preferences.set(PREFERENCE, locale);
}
