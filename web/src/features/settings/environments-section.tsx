import { Link } from "@tanstack/react-router";

import { PageError } from "@/features/delivery/page-error";
import { Section } from "@/features/delivery/section";
import { label, sha } from "@/features/delivery/text";
import { When } from "@/features/delivery/when";
import { useEnvironments } from "@/features/deployments/queries";
import type { DeployEnvironment } from "@/features/deployments/queries";
import { useT } from "@/i18n/use-t";
import { Badge } from "@/ui/badge";
import { Skeleton } from "@/ui/skeleton";

function Environment({
  projectId,
  environment,
}: {
  projectId: string;
  environment: DeployEnvironment;
}) {
  const t = useT();
  const { verification, last_succeeded: last } = environment;
  return (
    <li className="space-y-2 rounded-lg border border-border p-3 text-sm">
      <div className="flex flex-wrap items-center gap-2">
        <span className="font-medium break-words">{environment.name}</span>
        {verification.warnings.map((warning) => (
          <Badge key={warning} tone="attention">
            {label(t, "deploy.warning", warning)}
          </Badge>
        ))}
      </div>
      <dl className="grid gap-x-4 gap-y-1 sm:grid-cols-[max-content_1fr]">
        <dt className="text-muted-foreground">
          {t("settings.environments.repo")}
        </dt>
        <dd className="break-words">
          {t("deploy.env.branch", {
            repo: environment.repo.full_name,
            branch: environment.branch,
          })}
        </dd>
        <dt className="text-muted-foreground">
          {t("settings.environments.action")}
        </dt>
        <dd>{label(t, "deploy.action", environment.action)}</dd>
        <dt className="text-muted-foreground">
          {t("settings.environments.gate")}
        </dt>
        <dd>
          {environment.gate
            ? t("settings.environments.gateYes")
            : t("settings.environments.gateNo")}
        </dd>
        <dt className="text-muted-foreground">
          {t("settings.environments.onFailure")}
        </dt>
        <dd>{label(t, "deploy.onFailure", environment.on_failure)}</dd>
        <dt className="text-muted-foreground">
          {t("settings.environments.verification")}
        </dt>
        <dd>
          {t("settings.environments.verificationSummary", {
            ready: verification.ready ? "yes" : "no",
            commands: verification.commands,
          })}
        </dd>
        {environment.target_url !== null &&
          environment.target_url !== undefined && (
            <>
              <dt className="text-muted-foreground">
                {t("settings.environments.target")}
              </dt>
              <dd className="break-all">{environment.target_url}</dd>
            </>
          )}
        <dt className="text-muted-foreground">
          {t("settings.environments.lastSucceeded")}
        </dt>
        <dd className="flex flex-wrap items-center gap-x-2">
          {last === null || last === undefined ? (
            t("settings.environments.never")
          ) : (
            <>
              {last.sha !== null && last.sha !== undefined && (
                <code>{sha(last.sha)}</code>
              )}
              {last.finished_at !== null && last.finished_at !== undefined && (
                <When iso={last.finished_at} />
              )}
              <Link
                to="/deployments/$projectId"
                params={{ projectId }}
                search={{ environment: environment.name, deployment: last.id }}
                className="inline-flex min-h-11 items-center text-primary underline underline-offset-4"
              >
                {t("settings.environments.open")}
              </Link>
            </>
          )}
        </dd>
      </dl>
    </li>
  );
}

export function EnvironmentsSection({ projectId }: { projectId: string }) {
  const t = useT();
  const environments = useEnvironments(projectId);
  return (
    <Section title={t("settings.environments.title")}>
      {environments.isPending ? (
        <div
          role="status"
          aria-busy="true"
          aria-label={t("settings.environments.loading")}
          className="space-y-2"
        >
          <Skeleton className="h-10" />
          <Skeleton className="h-10" />
        </div>
      ) : environments.isError ? (
        <PageError
          title="settings.environments.error"
          error={environments.error}
          onRetry={() => void environments.refetch()}
        />
      ) : environments.data.length === 0 ? (
        <p className="text-sm text-muted-foreground">
          {t("settings.environments.empty")}
        </p>
      ) : (
        <ul className="space-y-2">
          {environments.data.map((environment) => (
            <Environment
              key={environment.name}
              projectId={projectId}
              environment={environment}
            />
          ))}
        </ul>
      )}
    </Section>
  );
}
