import { QueryClient } from "@tanstack/react-query";
import { RouterProvider, createRouter } from "@tanstack/react-router";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import { startEventStream } from "@/api/events";
import { signInAgain } from "@/features/auth/unauthorized";
import { platform } from "@/platform";

import { routeTree } from "./routeTree.gen";
import "./styles.css";

const queryClient = new QueryClient({
  defaultOptions: { queries: { staleTime: 5_000, retry: 1 } },
});
const router = createRouter({ routeTree, context: { queryClient } });

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

declare module "@tanstack/react-router" {
  interface Register {
    router: typeof router;
  }
}

startEventStream(queryClient);

const root = platform.rootElement();
if (root !== null) {
  createRoot(root).render(
    <StrictMode>
      <RouterProvider router={router} />
    </StrictMode>,
  );
}
