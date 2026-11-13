import { Link, useNavigate } from "@tanstack/react-router";
import { useId } from "react";

import { PageError } from "@/features/delivery/page-error";
import { useProject } from "@/features/delivery/queries";
import { label } from "@/features/delivery/text";
import { When } from "@/features/delivery/when";
import { useEnvironments } from "@/features/deployments/queries";
import { useT } from "@/i18n/use-t";
import { Badge } from "@/ui/badge";
import { Button } from "@/ui/button";
import { Card } from "@/ui/card";
import { cn } from "@/ui/cn";
import { Skeleton } from "@/ui/skeleton";

import { IncidentDetail } from "./incident-detail";
import { INCIDENT_STATES, SEVERITIES, useIncidents } from "./queries";
import type { Incident } from "./queries";
import { severityTone, stateTone } from "./text";

export type IncidentsSearch = {
  state?: string;
  severity?: string;
  environment?: string;
  incident?: string;
};

const PAGE = "mx-auto w-full max-w-6xl space-y-4 p-4";
const SELECT =
  "block min-h-11 w-full rounded-md border border-border bg-background px-2 text-sm";

function searchOf(
  filters: Omit<IncidentsSearch, "incident">,
  incident: string | undefined,
): IncidentsSearch {
  return {
    ...(filters.state === undefined ? {} : { state: filters.state }),
    ...(filters.severity === undefined ? {} : { severity: filters.severity }),
    ...(filters.environment === undefined
      ? {}
      : { environment: filters.environment }),
    ...(incident === undefined ? {} : { incident }),
  };
}

function Filters({
  projectId,
  search,
}: {
  projectId: string;
  search: IncidentsSearch;
}) {
  const t = useT();
  const navigate = useNavigate();
  const environments = useEnvironments(projectId);
  const stateId = useId();
  const severityId = useId();
  const environmentId = useId();
  const go = (next: Omit<IncidentsSearch, "incident">) => {
    void navigate({
      to: "/incidents/$projectId",
      params: { projectId },
      search: searchOf(
        {
          ...(next.state === "" ? {} : { state: next.state }),
          ...(next.severity === "" ? {} : { severity: next.severity }),
          ...(next.environment === "" ? {} : { environment: next.environment }),
        },
        search.incident,
      ),
    });
  };
  const current = {
    state: search.state ?? "",
    severity: search.severity ?? "",
    environment: search.environment ?? "",
  };
  return (
    <form
      aria-label={t("incident.filter.label")}
      className="grid gap-3 sm:grid-cols-3"
      onSubmit={(event) => {
        event.preventDefault();
      }}
    >
      <div className="space-y-1">
        <label htmlFor={stateId} className="text-sm font-medium">
          {t("incident.filter.state")}
        </label>
        <select
          id={stateId}
          value={current.state}
          onChange={(event) => {
            go({ ...current, state: event.target.value });
          }}
          className={SELECT}
        >
          <option value="">{t("incident.filter.all")}</option>
          {INCIDENT_STATES.map((state) => (
            <option key={state} value={state}>
              {label(t, "incident.state", state)}
            </option>
          ))}
        </select>
      </div>
      <div className="space-y-1">
        <label htmlFor={severityId} className="text-sm font-medium">
          {t("incident.filter.severity")}
        </label>
        <select
          id={severityId}
          value={current.severity}
          onChange={(event) => {
            go({ ...current, severity: event.target.value });
          }}
          className={SELECT}
        >
          <option value="">{t("incident.filter.all")}</option>
          {SEVERITIES.map((severity) => (
            <option key={severity} value={severity}>
              {label(t, "incident.severity", severity)}
            </option>
          ))}
        </select>
      </div>
      <div className="space-y-1">
        <label htmlFor={environmentId} className="text-sm font-medium">
          {t("incident.filter.environment")}
        </label>
        <select
          id={environmentId}
          value={current.environment}
          onChange={(event) => {
            go({ ...current, environment: event.target.value });
          }}
          className={SELECT}
        >
          <option value="">{t("incident.filter.all")}</option>
          {(environments.data ?? []).map((environment) => (
            <option key={environment.name} value={environment.name}>
              {environment.name}
            </option>
          ))}
        </select>
      </div>
    </form>
  );
}

function Row({
  projectId,
  incident,
  search,
}: {
  projectId: string;
  incident: Incident;
  search: IncidentsSearch;
}) {
  const t = useT();
  const selected = search.incident === incident.id;
  return (
    <li
      className={cn(
        "flex flex-wrap items-center gap-x-3 gap-y-1 p-3",
        selected && "bg-muted",
      )}
    >
      <Badge tone={severityTone(incident.severity)}>
        {label(t, "incident.severity", incident.severity)}
      </Badge>
      <Link
        to="/incidents/$projectId"
        params={{ projectId }}
        search={searchOf(search, incident.id)}
        aria-current={selected ? "true" : undefined}
        className="inline-flex min-h-11 items-center font-medium break-words text-primary underline underline-offset-4"
      >
        {incident.title}
      </Link>
      <Badge tone={stateTone(incident.state)}>
        {label(t, "incident.state", incident.state)}
      </Badge>
      <span className="text-sm break-words text-muted-foreground">
        {incident.environment ?? t("incident.detail.noEnvironment")}
      </span>
      <span className="text-sm text-muted-foreground">
        {t("incident.list.occurrences", { count: incident.occurrences })}
      </span>
      <span className="text-xs text-muted-foreground">
        {t("incident.list.lastSeen")} <When iso={incident.last_seen} />
      </span>
    </li>
  );
}

function History({
  projectId,
  search,
}: {
  projectId: string;
  search: IncidentsSearch;
}) {
  const t = useT();
  const incidents = useIncidents(projectId, search);
  if (incidents.isPending) {
    return (
      <div
        role="status"
        aria-busy="true"
        aria-label={t("incident.list.loading")}
      >
        <Skeleton className="h-40" />
      </div>
    );
  }
  if (incidents.isError) {
    return (
      <PageError
        title="incident.list.error"
        error={incidents.error}
        onRetry={() => void incidents.refetch()}
      />
    );
  }
  const items = incidents.data.pages.flatMap((page) => page.items);
  return (
    <Card className="overflow-hidden p-0">
      <h2 className="p-3 text-lg font-semibold">{t("incident.list.title")}</h2>
      {items.length === 0 ? (
        <p className="border-t border-border p-3 text-sm text-muted-foreground">
          {t("incident.list.empty")}
        </p>
      ) : (
        <ul className="divide-y divide-border border-t border-border">
          {items.map((incident) => (
            <Row
              key={incident.id}
              projectId={projectId}
              incident={incident}
              search={search}
            />
          ))}
        </ul>
      )}
      {incidents.hasNextPage && (
        <div className="border-t border-border p-3">
          <Button
            variant="outline"
            disabled={incidents.isFetchingNextPage}
            onClick={() => void incidents.fetchNextPage()}
          >
            {t("incident.list.more")}
          </Button>
        </div>
      )}
    </Card>
  );
}

export function IncidentsPage({
  projectId,
  search,
}: {
  projectId: string;
  search: IncidentsSearch;
}) {
  const t = useT();
  const project = useProject(projectId);
  return (
    <div className={PAGE}>
      <Link
        to="/delivery"
        className="inline-flex min-h-11 items-center text-sm font-medium text-primary underline underline-offset-4"
      >
        {t("incident.page.back")}
      </Link>
      <div>
        <h1 className="text-2xl font-semibold">{t("incident.page.title")}</h1>
        {project.data !== undefined && (
          <p className="text-sm break-words text-muted-foreground">
            {project.data.name}
          </p>
        )}
      </div>
      <Filters projectId={projectId} search={search} />
      <div className="grid gap-4 lg:grid-cols-2">
        <History projectId={projectId} search={search} />
        {search.incident !== undefined && (
          <IncidentDetail
            key={search.incident}
            incidentId={search.incident}
            projectId={projectId}
            close={
              <Link
                to="/incidents/$projectId"
                params={{ projectId }}
                search={searchOf(search, undefined)}
                className="inline-flex min-h-11 items-center text-sm font-medium text-primary underline underline-offset-4"
              >
                {t("incident.detail.close")}
              </Link>
            }
          />
        )}
      </div>
    </div>
  );
}
