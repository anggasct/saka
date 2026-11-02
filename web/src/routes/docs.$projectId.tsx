import { createFileRoute } from "@tanstack/react-router";

import { DocsPage } from "@/features/docs/docs-page";

type DocsSearch = { path?: string; draft?: string };

function nonEmpty(value: unknown): string | undefined {
  return typeof value === "string" && value !== "" ? value : undefined;
}

export const Route = createFileRoute("/docs/$projectId")({
  validateSearch: (search: Record<string, unknown>): DocsSearch => {
    const path = nonEmpty(search.path);
    const draft = nonEmpty(search.draft);
    return {
      ...(path === undefined ? {} : { path }),
      ...(path === undefined || draft === undefined ? {} : { draft }),
    };
  },
  component: DocsRoute,
});

function DocsRoute() {
  const { projectId } = Route.useParams();
  const { path, draft } = Route.useSearch();
  return (
    <DocsPage
      key={projectId}
      projectId={projectId}
      path={path}
      draftId={draft}
    />
  );
}
