import { QueryClientProvider } from "@tanstack/react-query";
import type { QueryClient } from "@tanstack/react-query";
import { useEffect, useMemo, useState } from "react";
import type { ReactNode } from "react";
import { IntlProvider } from "react-intl";

import { LocaleContext } from "@/i18n/locale-context";
import { rememberLocale } from "@/i18n/locale";
import { CATALOGS } from "@/i18n/messages";
import type { Locale } from "@/i18n/messages";
import { platform } from "@/platform";

export function Providers({
  queryClient,
  locale: initial,
  children,
}: {
  queryClient: QueryClient;
  locale: Locale;
  children: ReactNode;
}) {
  const [locale, setLocale] = useState(initial);
  useEffect(() => {
    platform.setDocumentLanguage(locale);
  }, [locale]);
  const state = useMemo(
    () => ({
      locale,
      setLocale: (next: Locale) => {
        rememberLocale(next);
        setLocale(next);
      },
    }),
    [locale],
  );
  return (
    <QueryClientProvider client={queryClient}>
      <LocaleContext value={state}>
        <IntlProvider
          locale={locale}
          defaultLocale="en"
          messages={CATALOGS[locale]}
        >
          {children}
        </IntlProvider>
      </LocaleContext>
    </QueryClientProvider>
  );
}
