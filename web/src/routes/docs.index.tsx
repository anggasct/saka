import { createFileRoute } from "@tanstack/react-router";

import { DocsIndexPage } from "@/features/docs/docs-index-page";

export const Route = createFileRoute("/docs/")({
  component: DocsIndexPage,
});
