import { createFileRoute } from "@tanstack/react-router";

import { ConnectionsPage } from "@/features/connections/connections-page";

type ConnectionsSearch = { reauth?: true };

export const Route = createFileRoute("/connections")({
  validateSearch: (search: Record<string, unknown>): ConnectionsSearch =>
    search.reauth === true || search.reauth === "true" ? { reauth: true } : {},
  component: ConnectionsRoute,
});

function ConnectionsRoute() {
  const { reauth } = Route.useSearch();
  return <ConnectionsPage mode={reauth === true ? "reauth" : "manage"} />;
}
