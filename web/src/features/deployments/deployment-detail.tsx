import { Link } from "@tanstack/react-router";
import { useId } from "react";
import type { ReactNode } from "react";

import { PageError } from "@/features/delivery/page-error";
import { sha, label } from "@/features/delivery/text";
import { When } from "@/features/delivery/when";
import { stateText } from "@/features/drafts/text";
import { useT } from "@/i18n/use-t";
import { Badge } from "@/ui/badge";
import { Card } from "@/ui/card";
import { Skeleton } from "@/ui/skeleton";

import {
  CancelDeploymentDialog,
  DraftFixDialog,
  RedeployDialog,
} from "./actions";
import { Evidence } from "./evidence";
import { useDeployment, useEnvironments } from "./queries";
import type { Deployment } from "./queries";
import { deployErrorText, gateTone, isCancellable, stateTone } from "./text";

function Facts({ deployment }: { deployment: Deployment }) {
  const t = useT();
  return (
    <dl className="grid grid-cols-1 gap-x-4 gap-y-1 text-sm sm:grid-cols-[max-content_1fr]">
      <dt className="text-muted-foreground">{t("deploy.detail.repo")}</dt>
      <dd className="break-words">{deployment.repo.full_name}</dd>
      <dt className="text-muted-foreground">{t("deploy.detail.branch")}</dt>
      <dd className="break-words">{deployment.branch}</dd>
      <dt className="text-muted-foreground">{t("deploy.detail.commit")}</dt>
      <dd>
        {deployment.sha === null || deployment.sha === undefined ? (
          t("deploy.list.notStarted")
        ) : (
          <code>{sha(deployment.sha)}</code>
        )}
      </dd>
      {deployment.previous_sha !== null &&
        deployment.previous_sha !== undefined && (
          <>
            <dt className="text-muted-foreground">
              {t("deploy.detail.previous")}
            </dt>
            <dd>
              <code>{sha(deployment.previous_sha)}</code>
            </dd>
          </>
        )}
      <dt className="text-muted-foreground">{t("deploy.detail.cause")}</dt>
      <dd>{label(t, "deploy.cause", deployment.cause)}</dd>
      {deployment.actor !== null && deployment.actor !== undefined && (
        <>
          <dt className="text-muted-foreground">{t("deploy.detail.actor")}</dt>
          <dd className="break-words">{deployment.actor}</dd>
        </>
      )}
      {deployment.reason !== null && deployment.reason !== undefined && (
        <>
          <dt className="text-muted-foreground">{t("deploy.detail.reason")}</dt>
          <dd className="break-words">{deployment.reason}</dd>
        </>
      )}
      <dt className="text-muted-foreground">{t("deploy.detail.queued")}</dt>
      <dd>
        <When iso={deployment.queued_at} />
      </dd>
      {deployment.started_at !== null &&
        deployment.started_at !== undefined && (
          <>
            <dt className="text-muted-foreground">
              {t("deploy.detail.started")}
            </dt>
            <dd>
              <When iso={deployment.started_at} />
            </dd>
          </>
        )}
      {deployment.finished_at !== null &&
        deployment.finished_at !== undefined && (
          <>
            <dt className="text-muted-foreground">
              {t("deploy.detail.finished")}
            </dt>
            <dd>
              <When iso={deployment.finished_at} />
            </dd>
          </>
        )}
    </dl>
  );
}

function Verification({ deployment }: { deployment: Deployment }) {
  const t = useT();
  const verification = deployment.verification;
  if (verification === null || verification === undefined) {
    return null;
  }
  const none =
    verification.commands.length === 0 &&
    (verification.ready === null || verification.ready === undefined);
  return (
    <section className="space-y-2">
      <h3 className="font-semibold">{t("deploy.verify.title")}</h3>
      {none ? (
        <p className="text-sm text-muted-foreground">
          {t("deploy.verify.none")}
        </p>
      ) : (
        <dl className="grid grid-cols-1 gap-x-4 gap-y-1 text-sm sm:grid-cols-[max-content_1fr]">
          {verification.ready !== null && verification.ready !== undefined && (
            <>
              <dt className="text-muted-foreground">
                {t("deploy.verify.ready")}
              </dt>
              <dd>
                <code className="break-all">{verification.ready}</code>
              </dd>
            </>
          )}
          {verification.commands.length > 0 && (
            <>
              <dt className="text-muted-foreground">
                {t("deploy.verify.commands")}
              </dt>
              <dd>
                <ul className="space-y-1">
                  {verification.commands.map((command, index) => (
                    <li key={`${index}:${command}`}>
                      <code className="break-all">{command}</code>
                    </li>
                  ))}
                </ul>
              </dd>
            </>
          )}
        </dl>
      )}
    </section>
  );
}

function Merges({ deployment }: { deployment: Deployment }) {
  const t = useT();
  const merges = deployment.merges ?? [];
  if (merges.length === 0) {
    return null;
  }
  return (
    <section className="space-y-2">
      <h3 className="font-semibold">{t("deploy.merges.title")}</h3>
      <ul className="space-y-1 text-sm">
        {merges.map((merge) => (
          <li
            key={merge.change_request_id}
            className="flex flex-wrap items-center gap-x-2"
          >
            <a
              className="inline-flex min-h-11 items-center text-primary underline underline-offset-4"
              href={merge.url}
              rel="noreferrer"
              target="_blank"
            >
              {t("task.change", { number: merge.number })}
            </a>
            {merge.merge_commit_sha !== null &&
              merge.merge_commit_sha !== undefined && (
                <span className="text-muted-foreground">
                  {t("deploy.merges.commit", {
                    sha: sha(merge.merge_commit_sha),
                  })}
                </span>
              )}
          </li>
        ))}
      </ul>
    </section>
  );
}

function Tasks({ deployment }: { deployment: Deployment }) {
  const t = useT();
  const tasks = deployment.tasks ?? [];
  return (
    <section className="space-y-2">
      <h3 className="font-semibold">{t("deploy.tasks.title")}</h3>
      {tasks.length === 0 ? (
        <p className="text-sm text-muted-foreground">
          {t("deploy.tasks.none")}
        </p>
      ) : (
        <ul className="space-y-1 text-sm">
          {tasks.map((task) => (
            <li key={task.id} className="flex flex-wrap items-center gap-2">
              <Link
                to="/delivery/tasks/$workItemId"
                params={{ workItemId: task.id }}
                className="inline-flex min-h-11 items-center break-all text-primary underline underline-offset-4"
              >
                {task.key}
              </Link>
              {task.gate === null || task.gate === undefined ? (
                <span className="text-muted-foreground">
                  {t("deploy.tasks.noGate")}
                </span>
              ) : (
                <Badge tone={gateTone(task.gate)}>
                  {label(t, "deploy.gate", task.gate)}
                </Badge>
              )}
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}

function FixDraft({
  deployment,
  projectId,
}: {
  deployment: Deployment;
  projectId: string;
}) {
  const t = useT();
  const draft = deployment.fix_draft;
  if (draft === null || draft === undefined) {
    return null;
  }
  return (
    <section className="space-y-2">
      <h3 className="font-semibold">{t("deploy.fix.title")}</h3>
      <p className="flex flex-wrap items-center gap-2 text-sm">
        <Link
          to="/docs/$projectId/drafts/$draftId"
          params={{ projectId, draftId: draft.id }}
          className="inline-flex min-h-11 items-center text-primary underline underline-offset-4"
        >
          {t("deploy.fix.open")}
        </Link>
        <Badge>{stateText(t, draft.state)}</Badge>
      </p>
    </section>
  );
}

function Detail({
  deployment,
  projectId,
  close,
}: {
  deployment: Deployment;
  projectId: string;
  close: ReactNode;
}) {
  const t = useT();
  const headingId = useId();
  const environments = useEnvironments(projectId);
  const defined = environments.data?.some(
    (e) => e.name === deployment.environment,
  );
  const failed = deployment.state === "failed";
  return (
    <Card aria-labelledby={headingId} className="space-y-4 p-4">
      <header className="flex flex-wrap items-start justify-between gap-2">
        <div className="space-y-1">
          <h2 id={headingId} className="text-lg font-semibold">
            {t("deploy.detail.title", { environment: deployment.environment })}
          </h2>
          <p className="flex flex-wrap items-center gap-2">
            <Badge tone={stateTone(deployment.state)}>
              {label(t, "deploy.state", deployment.state)}
            </Badge>
            {deployment.phase !== null && deployment.phase !== undefined && (
              <span className="text-sm text-muted-foreground">
                {t("deploy.detail.phase", {
                  failed: failed ? "yes" : "no",
                  phase: label(t, "deploy.phase", deployment.phase),
                })}
              </span>
            )}
          </p>
        </div>
        {close}
      </header>
      {deployment.error !== null && deployment.error !== undefined && (
        <p role="alert" className="text-sm break-words text-danger-foreground">
          {deployErrorText(t, deployment.error)}
        </p>
      )}
      <div className="flex flex-wrap gap-2">
        {defined === true && (
          <RedeployDialog
            projectId={projectId}
            environment={deployment.environment}
          />
        )}
        {isCancellable(deployment) && (
          <CancelDeploymentDialog deploymentId={deployment.id} />
        )}
        {failed && <DraftFixDialog deploymentId={deployment.id} />}
      </div>
      <Facts deployment={deployment} />
      <Verification deployment={deployment} />
      <section className="space-y-2">
        <h3 className="font-semibold">{t("deploy.evidence.title")}</h3>
        <Evidence checks={deployment.checks ?? []} />
      </section>
      <Merges deployment={deployment} />
      <Tasks deployment={deployment} />
      <FixDraft deployment={deployment} projectId={projectId} />
    </Card>
  );
}

export function DeploymentDetail({
  deploymentId,
  projectId,
  close,
}: {
  deploymentId: string;
  projectId: string;
  close: ReactNode;
}) {
  const t = useT();
  const deployment = useDeployment(deploymentId);
  if (deployment.isPending) {
    return (
      <div
        role="status"
        aria-busy="true"
        aria-label={t("deploy.detail.loading")}
      >
        <Skeleton className="h-48" />
      </div>
    );
  }
  if (deployment.isError) {
    return (
      <PageError
        title="deploy.detail.error"
        error={deployment.error}
        onRetry={() => void deployment.refetch()}
      />
    );
  }
  return (
    <Detail deployment={deployment.data} projectId={projectId} close={close} />
  );
}
