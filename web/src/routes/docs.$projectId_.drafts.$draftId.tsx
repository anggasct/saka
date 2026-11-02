import { createFileRoute } from "@tanstack/react-router";

import { DraftPage } from "@/features/drafts/draft-page";

export const Route = createFileRoute("/docs/$projectId_/drafts/$draftId")({
  component: DraftRoute,
});

function DraftRoute() {
  const { projectId, draftId } = Route.useParams();
  return <DraftPage key={draftId} projectId={projectId} draftId={draftId} />;
}
