import { createFileRoute } from "@tanstack/react-router";

import { TaskPage } from "@/features/delivery/task-page";

export const Route = createFileRoute("/delivery/tasks/$workItemId")({
  component: TaskRoute,
});

function TaskRoute() {
  const { workItemId } = Route.useParams();
  return <TaskPage key={workItemId} workItemId={workItemId} />;
}
