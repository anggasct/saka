import { createFileRoute } from "@tanstack/react-router";

import { LoginPage } from "@/features/auth/login-page";

type LoginSearch = { next?: string };

export const Route = createFileRoute("/login")({
  validateSearch: (search: Record<string, unknown>): LoginSearch =>
    typeof search.next === "string" ? { next: search.next } : {},
  component: LoginRoute,
});

function LoginRoute() {
  const { next } = Route.useSearch();
  return <LoginPage next={next} />;
}
