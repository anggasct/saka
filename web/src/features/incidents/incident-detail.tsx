import { Link } from "@tanstack/react-router";
import { useId } from "react";
import type { ReactNode } from "react";

import { PageError } from "@/features/delivery/page-error";
import { label, sha } from "@/features/delivery/text";
import { When } from "@/features/delivery/when";
import { stateTone as deployStateTone } from "@/features/deployments/text";
import { stateText } from "@/features/drafts/text";
import { useT } from "@/i18n/use-t";
import { Badge } from "@/ui/badge";
import { Card } from "@/ui/card";
import { Skeleton } from "@/ui/skeleton";

import {
  IncidentDismissDialog,
  IncidentDraftFixDialog,
  IncidentRevertDialog,
} from "./actions";
import { useIncident } from "./queries";
import type { IncidentDetail as Detail, IncidentSuspect } from "./queries";
import { httpUrl, labelEntries, severityTone, stateTone } from "./text";

const LINK =
  "inline-flex min-h-11 items-center text-primary underline underline-offset-4";

function Signal({ incident }: { incident: Detail }) {
  const t = useT();
  const url = httpUrl(incident.url ?? "");
  const labels = labelEntries(incident.labels);
  return (
    <section className="space-y-2">
      <h3 className="font-semibold">{t("incident.signal.title")}</h3>
      <p className="text-sm break-words whitespace-pre-wrap">
        {incident.summary === ""
          ? t("incident.signal.noSummary")
          : incident.summary}
      </p>
      {url !== null && (
        <a className={LINK} href={url} rel="noreferrer" target="_blank">
          {t("incident.signal.open")}
        </a>
      )}
      {labels.length > 0 && (
        <dl className="grid grid-cols-1 gap-x-4 gap-y-1 text-sm sm:grid-cols-[max-content_1fr]">
          {labels.map(([name, value]) => (
            <div key={name} className="contents">
              <dt className="break-all text-muted-foreground">{name}</dt>
              <dd className="break-all">{value}</dd>
            </div>
          ))}
        </dl>
      )}
    </section>
  );
}

function Facts({ incident }: { incident: Detail }) {
  const t = useT();
  return (
    <dl className="grid grid-cols-1 gap-x-4 gap-y-1 text-sm sm:grid-cols-[max-content_1fr]">
      <dt className="text-muted-foreground">{t("incident.detail.source")}</dt>
      <dd className="break-words">{incident.source}</dd>
      <dt className="text-muted-foreground">
        {t("incident.detail.environment")}
      </dt>
      <dd className="break-words">
        {incident.environment ?? t("incident.detail.noEnvironment")}
      </dd>
      <dt className="text-muted-foreground">{t("incident.detail.signal")}</dt>
      <dd>{label(t, "incident.signalStatus", incident.signal_status)}</dd>
      <dt className="text-muted-foreground">
        {t("incident.detail.occurrences")}
      </dt>
      <dd>{incident.occurrences}</dd>
      {incident.started_at !== null && incident.started_at !== undefined && (
        <>
          <dt className="text-muted-foreground">
            {t("incident.detail.started")}
          </dt>
          <dd>
            <When iso={incident.started_at} />
          </dd>
        </>
      )}
      <dt className="text-muted-foreground">
        {t("incident.detail.firstSeen")}
      </dt>
      <dd>
        <When iso={incident.first_seen} />
      </dd>
      <dt className="text-muted-foreground">{t("incident.detail.lastSeen")}</dt>
      <dd>
        <When iso={incident.last_seen} />
      </dd>
      {incident.resolved_at !== null && incident.resolved_at !== undefined && (
        <>
          <dt className="text-muted-foreground">
            {t("incident.detail.resolvedAt")}
          </dt>
          <dd>
            <When iso={incident.resolved_at} />
          </dd>
        </>
      )}
      {incident.dismiss_reason !== null &&
        incident.dismiss_reason !== undefined && (
          <>
            <dt className="text-muted-foreground">
              {t("incident.detail.dismissReason")}
            </dt>
            <dd className="break-words">{incident.dismiss_reason}</dd>
          </>
        )}
      {incident.actor !== null && incident.actor !== undefined && (
        <>
          <dt className="text-muted-foreground">
            {t("incident.detail.actor")}
          </dt>
          <dd className="break-words">{incident.actor}</dd>
        </>
      )}
    </dl>
  );
}

function DeploymentLink({
  incident,
  projectId,
}: {
  incident: Detail;
  projectId: string;
}) {
  const t = useT();
  const deployment = incident.deployment;
  if (deployment === null || deployment === undefined) {
    return (
      <section className="space-y-2">
        <h3 className="font-semibold">{t("incident.deployment.title")}</h3>
        <p className="text-sm text-muted-foreground">
          {t("incident.deployment.none")}
        </p>
      </section>
    );
  }
  const range = incident.range;
  return (
    <section className="space-y-2">
      <h3 className="font-semibold">{t("incident.deployment.title")}</h3>
      <p className="flex flex-wrap items-center gap-2 text-sm">
        <Badge tone={deployStateTone(deployment.state)}>
          {label(t, "deploy.state", deployment.state)}
        </Badge>
        {range !== null && range !== undefined && (
          <code>
            {t("incident.deployment.range", {
              known:
                range.previous_sha === null || range.previous_sha === undefined
                  ? "no"
                  : "yes",
              previous: sha(range.previous_sha),
              sha: sha(range.sha),
            })}
          </code>
        )}
        <Link
          to="/deployments/$projectId"
          params={{ projectId }}
          search={{ deployment: deployment.id }}
          className={LINK}
        >
          {t("incident.deployment.open")}
        </Link>
      </p>
    </section>
  );
}

function Suspect({
  suspect,
  incident,
  revertible,
}: {
  suspect: IncidentSuspect;
  incident: Detail;
  revertible: boolean;
}) {
  const t = useT();
  if (suspect.work_item !== null && suspect.work_item !== undefined) {
    const task = suspect.work_item;
    return (
      <li className="flex flex-wrap items-center gap-2">
        <Link
          to="/delivery/tasks/$workItemId"
          params={{ workItemId: task.id }}
          className={`${LINK} break-all`}
        >
          {task.key}
        </Link>
        {revertible && (
          <IncidentRevertDialog
            incidentId={incident.id}
            workItemId={task.id}
            taskKey={task.key}
          />
        )}
      </li>
    );
  }
  if (suspect.change !== null && suspect.change !== undefined) {
    return (
      <li className="flex flex-wrap items-center gap-2">
        <a
          className={LINK}
          href={suspect.change.url}
          rel="noreferrer"
          target="_blank"
        >
          {t("task.change", { number: suspect.change.number })}
        </a>
        <span className="text-sm text-muted-foreground">
          {t("incident.suspects.outside")}
        </span>
      </li>
    );
  }
  return null;
}

function Suspects({ incident }: { incident: Detail }) {
  const t = useT();
  return (
    <section className="space-y-2">
      <h3 className="font-semibold">{t("incident.suspects.title")}</h3>
      {incident.suspects.length === 0 ? (
        <p className="text-sm text-muted-foreground">
          {t("incident.suspects.none")}
        </p>
      ) : (
        <ul className="space-y-1 text-sm">
          {incident.suspects.map((suspect, index) => (
            <Suspect
              key={`${suspect.work_item?.id ?? suspect.change?.number ?? index}`}
              suspect={suspect}
              incident={incident}
              revertible={incident.state === "open"}
            />
          ))}
        </ul>
      )}
    </section>
  );
}

function Recovery({
  incident,
  projectId,
}: {
  incident: Detail;
  projectId: string;
}) {
  const t = useT();
  const { draft, spec, revert } = incident;
  const present =
    (draft !== null && draft !== undefined) ||
    (spec !== null && spec !== undefined) ||
    (revert !== null && revert !== undefined);
  if (!present) {
    return null;
  }
  return (
    <section className="space-y-2">
      <h3 className="font-semibold">{t("incident.recovery.title")}</h3>
      <ul className="space-y-1 text-sm">
        {draft !== null && draft !== undefined && (
          <li className="flex flex-wrap items-center gap-2">
            <Link
              to="/docs/$projectId/drafts/$draftId"
              params={{ projectId, draftId: draft.id }}
              className={LINK}
            >
              {t("incident.recovery.draft")}
            </Link>
            <Badge>{stateText(t, draft.state)}</Badge>
          </li>
        )}
        {spec !== null && spec !== undefined && (
          <li className="flex flex-wrap items-center gap-2">
            <Link
              to="/docs/$projectId"
              params={{ projectId }}
              search={{ path: spec.path }}
              className={`${LINK} break-all`}
            >
              {t("incident.recovery.spec", { path: spec.path })}
            </Link>
            <Badge>{label(t, "spec.state", spec.state)}</Badge>
          </li>
        )}
        {revert !== null && revert !== undefined && (
          <li className="flex flex-wrap items-center gap-2">
            <Link
              to="/delivery/tasks/$workItemId"
              params={{ workItemId: revert.id }}
              className={`${LINK} break-all`}
            >
              {t("incident.recovery.revert", { task: revert.key })}
            </Link>
            <Badge>{label(t, "task.state", revert.state)}</Badge>
          </li>
        )}
      </ul>
    </section>
  );
}

function Actions({ incident }: { incident: Detail }) {
  const t = useT();
  if (incident.state === "open") {
    return (
      <div className="flex flex-wrap gap-2">
        <IncidentDraftFixDialog incidentId={incident.id} />
        <IncidentDismissDialog incidentId={incident.id} />
      </div>
    );
  }
  if (incident.state === "handling") {
    return (
      <p className="text-sm text-muted-foreground">
        {t("incident.detail.handling")}
      </p>
    );
  }
  return null;
}

function Body({
  incident,
  projectId,
  close,
}: {
  incident: Detail;
  projectId: string;
  close: ReactNode;
}) {
  const t = useT();
  const headingId = useId();
  return (
    <Card aria-labelledby={headingId} className="space-y-4 p-4">
      <header className="flex flex-wrap items-start justify-between gap-2">
        <div className="space-y-1">
          <h2 id={headingId} className="text-lg font-semibold break-words">
            {incident.title}
          </h2>
          <p className="flex flex-wrap items-center gap-2">
            <Badge tone={severityTone(incident.severity)}>
              {label(t, "incident.severity", incident.severity)}
            </Badge>
            <Badge tone={stateTone(incident.state)}>
              {label(t, "incident.state", incident.state)}
            </Badge>
            {incident.resolution !== null &&
              incident.resolution !== undefined && (
                <Badge>
                  {label(t, "incident.resolution", incident.resolution)}
                </Badge>
              )}
          </p>
        </div>
        {close}
      </header>
      <Actions incident={incident} />
      <Signal incident={incident} />
      <Facts incident={incident} />
      <DeploymentLink incident={incident} projectId={projectId} />
      <Suspects incident={incident} />
      <Recovery incident={incident} projectId={projectId} />
    </Card>
  );
}

export function IncidentDetail({
  incidentId,
  projectId,
  close,
}: {
  incidentId: string;
  projectId: string;
  close: ReactNode;
}) {
  const t = useT();
  const incident = useIncident(incidentId);
  if (incident.isPending) {
    return (
      <div
        role="status"
        aria-busy="true"
        aria-label={t("incident.detail.loading")}
      >
        <Skeleton className="h-48" />
      </div>
    );
  }
  if (incident.isError) {
    return (
      <PageError
        title="incident.detail.error"
        error={incident.error}
        onRetry={() => void incident.refetch()}
      />
    );
  }
  return <Body incident={incident.data} projectId={projectId} close={close} />;
}
