import { createFileRoute } from "@tanstack/react-router";

import { IncidentsPage } from "@/features/incidents/incidents-page";
import type { IncidentsSearch } from "@/features/incidents/incidents-page";
import { INCIDENT_STATES, SEVERITIES } from "@/features/incidents/queries";

function nonEmpty(value: unknown): string | undefined {
  return typeof value === "string" && value !== "" ? value : undefined;
}

function oneOf(value: unknown, allowed: readonly string[]): string | undefined {
  const found = nonEmpty(value);
  return found !== undefined && allowed.includes(found) ? found : undefined;
}

export const Route = createFileRoute("/incidents/$projectId")({
  validateSearch: (search: Record<string, unknown>): IncidentsSearch => {
    const state = oneOf(search.state, INCIDENT_STATES);
    const severity = oneOf(search.severity, SEVERITIES);
    const environment = nonEmpty(search.environment);
    const incident = nonEmpty(search.incident);
    return {
      ...(state === undefined ? {} : { state }),
      ...(severity === undefined ? {} : { severity }),
      ...(environment === undefined ? {} : { environment }),
      ...(incident === undefined ? {} : { incident }),
    };
  },
  component: IncidentsRoute,
});

function IncidentsRoute() {
  const { projectId } = Route.useParams();
  const search = Route.useSearch();
  return (
    <IncidentsPage key={projectId} projectId={projectId} search={search} />
  );
}
