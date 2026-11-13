import { createFileRoute } from "@tanstack/react-router";

import { DeploymentsPage } from "@/features/deployments/deployments-page";
import type { DeploymentsSearch } from "@/features/deployments/deployments-page";
import { DEPLOYMENT_STATES } from "@/features/deployments/queries";

function nonEmpty(value: unknown): string | undefined {
  return typeof value === "string" && value !== "" ? value : undefined;
}

export const Route = createFileRoute("/deployments/$projectId")({
  validateSearch: (search: Record<string, unknown>): DeploymentsSearch => {
    const environment = nonEmpty(search.environment);
    const state = nonEmpty(search.state);
    const deployment = nonEmpty(search.deployment);
    return {
      ...(environment === undefined ? {} : { environment }),
      ...(state === undefined ||
      !(DEPLOYMENT_STATES as readonly string[]).includes(state)
        ? {}
        : { state }),
      ...(deployment === undefined ? {} : { deployment }),
    };
  },
  component: DeploymentsRoute,
});

function DeploymentsRoute() {
  const { projectId } = Route.useParams();
  const search = Route.useSearch();
  return (
    <DeploymentsPage key={projectId} projectId={projectId} search={search} />
  );
}
