import { createFileRoute, redirect } from "@tanstack/react-router";

export const Route = createFileRoute("/")({
  beforeLoad: () => {
    // TanStack Router redirects by throwing.
    // eslint-disable-next-line @typescript-eslint/only-throw-error -- the router's redirect contract
    throw redirect({ to: "/delivery" });
  },
});
