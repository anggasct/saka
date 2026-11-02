import { createFileRoute } from "@tanstack/react-router";

import { DocsPage } from "@/features/docs/docs-page";

type DocsSearch = { path?: string };

export const Route = createFileRoute("/docs/$projectId")({
  validateSearch: (search: Record<string, unknown>): DocsSearch =>
    typeof search.path === "string" && search.path !== ""
      ? { path: search.path }
      : {},
  component: DocsRoute,
});

function DocsRoute() {
  const { projectId } = Route.useParams();
  const { path } = Route.useSearch();
  return <DocsPage key={projectId} projectId={projectId} path={path} />;
}
