import { createFileRoute } from "@tanstack/react-router";

import { SessionPage } from "@/features/sessions/session-page";

export const Route = createFileRoute("/sessions/$sessionId")({
  component: SessionRoute,
});

function SessionRoute() {
  const { sessionId } = Route.useParams();
  return <SessionPage key={sessionId} sessionId={sessionId} />;
}
