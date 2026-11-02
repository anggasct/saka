import { createContext, useContext } from "react";

import type { Locale } from "./messages";

export type LocaleState = {
  locale: Locale;
  setLocale: (locale: Locale) => void;
};

export const LocaleContext = createContext<LocaleState>({
  locale: "en",
  setLocale: () => undefined,
});

export function useLocale(): LocaleState {
  return useContext(LocaleContext);
}
