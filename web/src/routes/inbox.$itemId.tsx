import { createFileRoute } from "@tanstack/react-router";

import { InboxPage } from "@/features/inbox/inbox-page";

export const Route = createFileRoute("/inbox/$itemId")({
  component: InboxItemRoute,
});

function InboxItemRoute() {
  const { itemId } = Route.useParams();
  return <InboxPage focus={itemId} />;
}
