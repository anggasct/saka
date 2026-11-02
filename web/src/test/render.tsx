import { QueryClient } from "@tanstack/react-query";
import {
  RouterProvider,
  createMemoryHistory,
  createRootRoute,
  createRouter,
} from "@tanstack/react-router";
import { render } from "@testing-library/react";

import { routeTree } from "@/routeTree.gen";
import type { ReactElement } from "react";

import type { Locale } from "@/i18n/messages";
import { Providers } from "@/providers";

export function renderWithProviders(ui: ReactElement, locale: Locale = "en") {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  const router = createRouter({
    routeTree: createRootRoute({ component: () => ui }),
    history: createMemoryHistory({ initialEntries: ["/"] }),
  });
  const result = render(
    <Providers queryClient={queryClient} locale={locale}>
      <RouterProvider router={router} />
    </Providers>,
  );
  return { ...result, queryClient, router };
}

export function renderApp(url: string) {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  const router = createRouter({
    routeTree,
    history: createMemoryHistory({ initialEntries: [url] }),
    context: { queryClient },
  });
  const result = render(<RouterProvider router={router} />);
  return { ...result, queryClient, router };
}
