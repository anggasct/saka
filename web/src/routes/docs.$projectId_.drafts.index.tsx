import { createFileRoute } from "@tanstack/react-router";

import { DraftsPage } from "@/features/drafts/drafts-page";

type DraftsSearch = { state?: "all" };

export const Route = createFileRoute("/docs/$projectId_/drafts/")({
  validateSearch: (search: Record<string, unknown>): DraftsSearch =>
    search.state === "all" ? { state: "all" } : {},
  component: DraftsRoute,
});

function DraftsRoute() {
  const { projectId } = Route.useParams();
  const { state } = Route.useSearch();
  return (
    <DraftsPage
      key={projectId}
      projectId={projectId}
      filter={state ?? "active"}
    />
  );
}
