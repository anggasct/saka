import { QueryClient } from "@tanstack/react-query";
import { render } from "@testing-library/react";
import type { ReactElement } from "react";

import type { Locale } from "@/i18n/messages";
import { Providers } from "@/providers";

export function renderWithProviders(ui: ReactElement, locale: Locale = "en") {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  const result = render(
    <Providers queryClient={queryClient} locale={locale}>
      {ui}
    </Providers>,
  );
  return { ...result, queryClient };
}
