import { QueryClientProvider } from "@tanstack/react-query";
import type { QueryClient } from "@tanstack/react-query";
import type { ReactNode } from "react";
import { IntlProvider } from "react-intl";

import { CATALOGS } from "@/i18n/messages";
import type { Locale } from "@/i18n/messages";

export function Providers({
  queryClient,
  locale,
  children,
}: {
  queryClient: QueryClient;
  locale: Locale;
  children: ReactNode;
}) {
  return (
    <QueryClientProvider client={queryClient}>
      <IntlProvider
        locale={locale}
        defaultLocale="en"
        messages={CATALOGS[locale]}
      >
        {children}
      </IntlProvider>
    </QueryClientProvider>
  );
}
