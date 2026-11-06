import { createFileRoute } from "@tanstack/react-router";

import { SetupPage } from "@/features/auth/setup-page";

type SetupSearch = { next?: string };

export const Route = createFileRoute("/setup")({
  validateSearch: (search: Record<string, unknown>): SetupSearch =>
    typeof search.next === "string" ? { next: search.next } : {},
  component: SetupRoute,
});

function SetupRoute() {
  const { next } = Route.useSearch();
  return <SetupPage next={next} />;
}
