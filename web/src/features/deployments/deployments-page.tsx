import { Link, useNavigate } from "@tanstack/react-router";
import { useId } from "react";

import { PageError } from "@/features/delivery/page-error";
import { useProject } from "@/features/delivery/queries";
import { label, sha } from "@/features/delivery/text";
import { When } from "@/features/delivery/when";
import { useT } from "@/i18n/use-t";
import { Badge } from "@/ui/badge";
import { Button } from "@/ui/button";
import { Card } from "@/ui/card";
import { cn } from "@/ui/cn";
import { Skeleton } from "@/ui/skeleton";

import { RedeployDialog } from "./actions";
import { DeploymentDetail } from "./deployment-detail";
import { DEPLOYMENT_STATES, useDeployments, useEnvironments } from "./queries";
import type { DeployEnvironment, Deployment } from "./queries";
import { stateTone } from "./text";

export type DeploymentsSearch = {
  environment?: string;
  state?: string;
  deployment?: string;
};

const PAGE = "mx-auto w-full max-w-6xl space-y-4 p-4";
const SELECT =
  "block min-h-11 w-full rounded-md border border-border bg-background px-2 text-sm";

function searchOf(
  environment: string | undefined,
  state: string | undefined,
  deployment: string | undefined,
): DeploymentsSearch {
  return {
    ...(environment === undefined ? {} : { environment }),
    ...(state === undefined ? {} : { state }),
    ...(deployment === undefined ? {} : { deployment }),
  };
}

function Summary({
  title,
  summary,
}: {
  title: string;
  summary: NonNullable<DeployEnvironment["active"]>;
}) {
  const t = useT();
  return (
    <p className="flex flex-wrap items-center gap-2 text-sm">
      <span className="text-muted-foreground">{title}</span>
      <Badge tone={stateTone(summary.state)}>
        {label(t, "deploy.state", summary.state)}
      </Badge>
      {summary.sha !== null && summary.sha !== undefined && (
        <code>{sha(summary.sha)}</code>
      )}
      {summary.finished_at !== null && summary.finished_at !== undefined && (
        <span className="text-xs text-muted-foreground">
          <When iso={summary.finished_at} />
        </span>
      )}
    </p>
  );
}

function EnvironmentCard({
  projectId,
  environment,
}: {
  projectId: string;
  environment: DeployEnvironment;
}) {
  const t = useT();
  return (
    <li className="space-y-2 rounded-lg border border-border p-3">
      <h3 className="font-medium break-words">{environment.name}</h3>
      <p className="text-sm break-words text-muted-foreground">
        {t("deploy.env.branch", {
          repo: environment.repo.name,
          branch: environment.branch,
        })}
      </p>
      {environment.active !== null && environment.active !== undefined && (
        <Summary title={t("deploy.env.active")} summary={environment.active} />
      )}
      {environment.queued !== null && environment.queued !== undefined && (
        <Summary title={t("deploy.env.queued")} summary={environment.queued} />
      )}
      {environment.last_succeeded !== null &&
      environment.last_succeeded !== undefined ? (
        <Summary
          title={t("deploy.env.lastSucceeded")}
          summary={environment.last_succeeded}
        />
      ) : (
        <p className="text-sm text-muted-foreground">
          {t("deploy.env.neverSucceeded")}
        </p>
      )}
      <RedeployDialog projectId={projectId} environment={environment.name} />
    </li>
  );
}

function Environments({ projectId }: { projectId: string }) {
  const t = useT();
  const environments = useEnvironments(projectId);
  if (environments.isPending) {
    return (
      <div role="status" aria-busy="true" aria-label={t("deploy.env.loading")}>
        <Skeleton className="h-24" />
      </div>
    );
  }
  if (environments.isError) {
    return (
      <PageError
        title="deploy.env.error"
        error={environments.error}
        onRetry={() => void environments.refetch()}
      />
    );
  }
  return (
    <Card className="space-y-3 p-4">
      <h2 className="text-lg font-semibold">{t("deploy.env.title")}</h2>
      {environments.data.length === 0 ? (
        <p className="text-sm text-muted-foreground">{t("deploy.env.none")}</p>
      ) : (
        <ul className="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
          {environments.data.map((environment) => (
            <EnvironmentCard
              key={environment.name}
              projectId={projectId}
              environment={environment}
            />
          ))}
        </ul>
      )}
    </Card>
  );
}

function Filters({
  projectId,
  search,
}: {
  projectId: string;
  search: DeploymentsSearch;
}) {
  const t = useT();
  const navigate = useNavigate();
  const environments = useEnvironments(projectId);
  const environmentId = useId();
  const stateId = useId();
  const go = (environment: string, state: string) => {
    void navigate({
      to: "/deployments/$projectId",
      params: { projectId },
      search: searchOf(
        environment === "" ? undefined : environment,
        state === "" ? undefined : state,
        search.deployment,
      ),
    });
  };
  return (
    <form
      aria-label={t("deploy.filter.label")}
      className="grid gap-3 sm:grid-cols-2"
      onSubmit={(event) => {
        event.preventDefault();
      }}
    >
      <div className="space-y-1">
        <label htmlFor={environmentId} className="text-sm font-medium">
          {t("deploy.filter.environment")}
        </label>
        <select
          id={environmentId}
          value={search.environment ?? ""}
          onChange={(event) => {
            go(event.target.value, search.state ?? "");
          }}
          className={SELECT}
        >
          <option value="">{t("deploy.filter.all")}</option>
          {(environments.data ?? []).map((environment) => (
            <option key={environment.name} value={environment.name}>
              {environment.name}
            </option>
          ))}
        </select>
      </div>
      <div className="space-y-1">
        <label htmlFor={stateId} className="text-sm font-medium">
          {t("deploy.filter.state")}
        </label>
        <select
          id={stateId}
          value={search.state ?? ""}
          onChange={(event) => {
            go(search.environment ?? "", event.target.value);
          }}
          className={SELECT}
        >
          <option value="">{t("deploy.filter.all")}</option>
          {DEPLOYMENT_STATES.map((state) => (
            <option key={state} value={state}>
              {label(t, "deploy.state", state)}
            </option>
          ))}
        </select>
      </div>
    </form>
  );
}

function Row({
  projectId,
  deployment,
  search,
}: {
  projectId: string;
  deployment: Deployment;
  search: DeploymentsSearch;
}) {
  const t = useT();
  const selected = search.deployment === deployment.id;
  return (
    <li
      className={cn(
        "flex flex-wrap items-center gap-x-3 gap-y-1 p-3",
        selected && "bg-muted",
      )}
    >
      <Link
        to="/deployments/$projectId"
        params={{ projectId }}
        search={searchOf(search.environment, search.state, deployment.id)}
        aria-current={selected ? "true" : undefined}
        className="inline-flex min-h-11 items-center font-medium break-all text-primary underline underline-offset-4"
      >
        {deployment.environment}
      </Link>
      <Badge tone={stateTone(deployment.state)}>
        {label(t, "deploy.state", deployment.state)}
      </Badge>
      <code className="text-sm">
        {deployment.sha === null || deployment.sha === undefined
          ? t("deploy.list.notStarted")
          : sha(deployment.sha)}
      </code>
      <span className="text-sm text-muted-foreground">
        {label(t, "deploy.cause", deployment.cause)}
      </span>
      <span className="text-xs text-muted-foreground">
        <When iso={deployment.queued_at} />
      </span>
    </li>
  );
}

function History({
  projectId,
  search,
}: {
  projectId: string;
  search: DeploymentsSearch;
}) {
  const t = useT();
  const deployments = useDeployments(
    projectId,
    search.environment,
    search.state,
  );
  if (deployments.isPending) {
    return (
      <div role="status" aria-busy="true" aria-label={t("deploy.list.loading")}>
        <Skeleton className="h-40" />
      </div>
    );
  }
  if (deployments.isError) {
    return (
      <PageError
        title="deploy.list.error"
        error={deployments.error}
        onRetry={() => void deployments.refetch()}
      />
    );
  }
  const items = deployments.data.pages.flatMap((page) => page.items);
  return (
    <Card className="overflow-hidden p-0">
      <h2 className="p-3 text-lg font-semibold">{t("deploy.list.title")}</h2>
      {items.length === 0 ? (
        <p className="border-t border-border p-3 text-sm text-muted-foreground">
          {t("deploy.list.empty")}
        </p>
      ) : (
        <ul className="divide-y divide-border border-t border-border">
          {items.map((deployment) => (
            <Row
              key={deployment.id}
              projectId={projectId}
              deployment={deployment}
              search={search}
            />
          ))}
        </ul>
      )}
      {deployments.hasNextPage && (
        <div className="border-t border-border p-3">
          <Button
            variant="outline"
            disabled={deployments.isFetchingNextPage}
            onClick={() => void deployments.fetchNextPage()}
          >
            {t("deploy.list.more")}
          </Button>
        </div>
      )}
    </Card>
  );
}

export function DeploymentsPage({
  projectId,
  search,
}: {
  projectId: string;
  search: DeploymentsSearch;
}) {
  const t = useT();
  const project = useProject(projectId);
  return (
    <div className={PAGE}>
      <Link
        to="/delivery"
        className="inline-flex min-h-11 items-center text-sm font-medium text-primary underline underline-offset-4"
      >
        {t("deploy.page.back")}
      </Link>
      <div>
        <h1 className="text-2xl font-semibold">{t("deploy.page.title")}</h1>
        {project.data !== undefined && (
          <p className="text-sm break-words text-muted-foreground">
            {project.data.name}
          </p>
        )}
      </div>
      <Environments projectId={projectId} />
      <Filters projectId={projectId} search={search} />
      <div className="grid gap-4 lg:grid-cols-2">
        <History projectId={projectId} search={search} />
        {search.deployment !== undefined && (
          <DeploymentDetail
            key={search.deployment}
            deploymentId={search.deployment}
            projectId={projectId}
            close={
              <Link
                to="/deployments/$projectId"
                params={{ projectId }}
                search={searchOf(search.environment, search.state, undefined)}
                className="inline-flex min-h-11 items-center text-sm font-medium text-primary underline underline-offset-4"
              >
                {t("deploy.detail.close")}
              </Link>
            }
          />
        )}
      </div>
    </div>
  );
}
