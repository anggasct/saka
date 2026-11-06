import { QueryClient } from "@tanstack/react-query";
import { RouterProvider, createRouter } from "@tanstack/react-router";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import { startEventStream } from "@/api/events";
import { checkServer } from "@/api/meta";
import { signInAgain } from "@/features/auth/unauthorized";
import { ConnectionsPage } from "@/features/connections/connections-page";
import { IncompatiblePage } from "@/features/connection/incompatible-page";
import { initialLocale } from "@/i18n/locale";
import { platform } from "@/platform";
import { Providers } from "@/providers";

import { routeTree } from "./routeTree.gen";

const queryClient = new QueryClient({
  defaultOptions: { queries: { staleTime: 5_000, retry: 1 } },
});
const router = createRouter({ routeTree, context: { queryClient } });

declare module "@tanstack/react-router" {
  interface Register {
    router: typeof router;
  }
}

function watchSignIn() {
  const signIn = signInAgain(router);
  queryClient.getQueryCache().subscribe((event) => {
    if (event.type === "updated" && event.action.type === "error") {
      signIn(event.action.error);
    }
  });
  queryClient.getMutationCache().subscribe((event) => {
    if (event.type === "updated" && event.action.type === "error") {
      signIn(event.action.error);
    }
  });
}

export async function start() {
  const root = platform.rootElement();
  if (root === null) {
    return;
  }
  const shell = platform.connections();
  if (shell !== null && !shell.hasActive()) {
    createRoot(root).render(
      <StrictMode>
        <Providers queryClient={queryClient} locale={initialLocale()}>
          <ConnectionsPage mode="setup" />
        </Providers>
      </StrictMode>,
    );
    return;
  }
  const server = await checkServer();
  if (server.status === "incompatible") {
    createRoot(root).render(
      <StrictMode>
        <Providers queryClient={queryClient} locale={initialLocale()}>
          <IncompatiblePage problem={server} />
        </Providers>
      </StrictMode>,
    );
    return;
  }
  watchSignIn();
  platform.onNavigate((path) => {
    if (path.startsWith("/") && !path.startsWith("//")) {
      router.history.push(path);
    }
  });
  startEventStream(queryClient);
  if (import.meta.env.PROD) {
    platform.registerServiceWorker("/sw.js");
  }
  createRoot(root).render(
    <StrictMode>
      <RouterProvider router={router} />
    </StrictMode>,
  );
}
