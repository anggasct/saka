import { PageError } from "@/features/delivery/page-error";
import { Section } from "@/features/delivery/section";
import { label } from "@/features/delivery/text";
import { useIncidentSources } from "@/features/incidents/queries";
import type { IncidentSource } from "@/features/incidents/queries";
import { useT } from "@/i18n/use-t";
import { Skeleton } from "@/ui/skeleton";

function Source({ source }: { source: IncidentSource }) {
  const t = useT();
  return (
    <li className="space-y-2 rounded-lg border border-border p-3 text-sm">
      <span className="font-medium break-words">{source.name}</span>
      <dl className="grid gap-x-4 gap-y-1 sm:grid-cols-[max-content_1fr]">
        <dt className="text-muted-foreground">
          {t("settings.incidentSources.kind")}
        </dt>
        <dd>{label(t, "incident.sourceKind", source.kind)}</dd>
        <dt className="text-muted-foreground">
          {t("settings.incidentSources.endpoint")}
        </dt>
        <dd>
          <code className="break-all">
            {source.endpoint ?? source.endpoint_path}
          </code>
        </dd>
        <dt className="text-muted-foreground">
          {t("settings.incidentSources.action")}
        </dt>
        <dd>
          {t("settings.incidentSources.actionSummary", {
            action: source.action,
            severity: label(t, "incident.severity", source.min_severity),
          })}
        </dd>
        <dt className="text-muted-foreground">
          {t("settings.incidentSources.open")}
        </dt>
        <dd>
          {t("settings.incidentSources.openSummary", {
            open: source.open,
            max: source.max_open,
          })}
        </dd>
        {source.repo !== null && source.repo !== undefined && (
          <>
            <dt className="text-muted-foreground">
              {t("settings.incidentSources.repo")}
            </dt>
            <dd className="break-words">{source.repo}</dd>
          </>
        )}
      </dl>
    </li>
  );
}

export function IncidentSourcesSection({ projectId }: { projectId: string }) {
  const t = useT();
  const sources = useIncidentSources(projectId);
  return (
    <Section title={t("settings.incidentSources.title")}>
      {sources.isPending ? (
        <div
          role="status"
          aria-busy="true"
          aria-label={t("settings.incidentSources.loading")}
          className="space-y-2"
        >
          <Skeleton className="h-10" />
          <Skeleton className="h-10" />
        </div>
      ) : sources.isError ? (
        <PageError
          title="settings.incidentSources.error"
          error={sources.error}
          onRetry={() => void sources.refetch()}
        />
      ) : sources.data.length === 0 ? (
        <p className="text-sm text-muted-foreground">
          {t("settings.incidentSources.empty")}
        </p>
      ) : (
        <ul className="space-y-2">
          {sources.data.map((source) => (
            <Source key={source.name} source={source} />
          ))}
        </ul>
      )}
    </Section>
  );
}
