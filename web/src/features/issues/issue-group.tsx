import { useQueryClient } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";

import { QUERY_ROOTS } from "@/api/events";
import { ActionDialog } from "@/features/delivery/action-dialog";
import { PageError } from "@/features/delivery/page-error";
import type { ProjectItem } from "@/features/delivery/queries";
import { label } from "@/features/delivery/text";
import { useT } from "@/i18n/use-t";
import { Badge } from "@/ui/badge";
import { Skeleton } from "@/ui/skeleton";

import { AddIssueDialog } from "./add-issue-dialog";
import type { IssueIntake } from "./queries";
import {
  dismissIssueIntake,
  retryIssueIntake,
  useIssueIntakes,
} from "./queries";
import { canDismiss, canRetry, stateTone } from "./text";

const LINK =
  "inline-flex min-h-11 items-center text-sm font-medium text-primary underline underline-offset-4";

function useRefresh() {
  const queryClient = useQueryClient();
  return () =>
    Promise.all(
      [
        QUERY_ROOTS.issueIntakes,
        QUERY_ROOTS.inbox,
        QUERY_ROOTS.specs,
        QUERY_ROOTS.specDrafts,
      ].map((root) => queryClient.invalidateQueries({ queryKey: [root] })),
    );
}

function Actions({ intake }: { intake: IssueIntake }) {
  const t = useT();
  const refresh = useRefresh();
  return (
    <div className="flex flex-wrap gap-2">
      {canRetry(intake) && (
        <ActionDialog
          trigger={t("issues.retry.trigger")}
          triggerVariant="default"
          title={t("issues.retry.title")}
          description={t("issues.retry.description", {
            left: 3 - intake.retries,
          })}
          submit={t("issues.retry.submit")}
          onSubmit={async (_, key) => {
            await retryIssueIntake(intake.id, key);
            await refresh();
          }}
        />
      )}
      {canDismiss(intake) && (
        <ActionDialog
          trigger={t("issues.dismiss.trigger")}
          title={t("issues.dismiss.title")}
          description={t("issues.dismiss.description")}
          reason={{ label: t("issues.dismiss.reason"), required: false }}
          submit={t("issues.dismiss.submit")}
          onSubmit={async (reason, key) => {
            await dismissIssueIntake(intake.id, reason, key);
            await refresh();
          }}
        />
      )}
    </div>
  );
}

function IntakeRow({ intake }: { intake: IssueIntake }) {
  const t = useT();
  return (
    <li className="flex flex-col gap-2 border-t border-border p-4 first:border-t-0 sm:flex-row sm:items-start sm:gap-4">
      <div className="flex shrink-0 flex-wrap items-center gap-2 sm:w-40 sm:flex-col sm:items-start">
        <a
          className={LINK}
          href={intake.issue.url}
          rel="noreferrer"
          target="_blank"
        >
          {t("issues.number", { number: intake.issue.number })}
        </a>
        <Badge tone={stateTone(intake.state)}>
          {label(t, "issues.state", intake.state)}
        </Badge>
      </div>
      <div className="min-w-0 flex-1 space-y-1">
        <p className="break-words">{intake.issue.title}</p>
        <p className="flex flex-wrap items-center gap-2 text-sm text-muted-foreground">
          <Badge>{label(t, "kind", intake.kind)}</Badge>
          <Badge>{label(t, "issues.lane", intake.lane)}</Badge>
          <span>{intake.repo.full_name}</span>
        </p>
        {intake.spec != null && (
          <Link
            to="/docs/$projectId"
            params={{ projectId: intake.project_id }}
            search={{ path: intake.spec.path }}
            className={`${LINK} break-all`}
          >
            {t("issues.spec", { path: intake.spec.path })}
          </Link>
        )}
        {intake.spec == null && intake.draft != null && (
          <p className="text-sm text-muted-foreground">
            {t("issues.draft", {
              state: label(t, "draft.state", intake.draft.state),
            })}
          </p>
        )}
        {intake.failure != null && (
          <p className="text-sm text-danger-foreground">
            {t("issues.failure", { code: intake.failure.code })}
          </p>
        )}
      </div>
      <Actions intake={intake} />
    </li>
  );
}

export function IssueGroup({ project }: { project: ProjectItem }) {
  const t = useT();
  const entry = project.workflow.issue;
  const intakes = useIssueIntakes(project.id, entry != null);
  if (entry == null) {
    return null;
  }
  const headingId = `issues-${project.id}`;
  const items = intakes.data ?? [];
  return (
    <section aria-labelledby={headingId} className="border-t border-border">
      <header className="flex flex-wrap items-center justify-between gap-2 px-4 pt-4">
        <h3 id={headingId} className="font-medium break-words">
          {t("issues.heading")}
        </h3>
        <AddIssueDialog project={project} />
      </header>
      {intakes.error !== null ? (
        <div className="p-4">
          <PageError
            title="issues.error.title"
            error={intakes.error}
            onRetry={() => {
              void intakes.refetch();
            }}
          />
        </div>
      ) : intakes.isPending ? (
        <div
          aria-busy="true"
          aria-label={t("issues.loading")}
          role="status"
          className="space-y-2 p-4"
        >
          <Skeleton className="h-6" />
          <Skeleton className="h-6" />
        </div>
      ) : items.length === 0 ? (
        <p className="p-4 text-sm text-muted-foreground">
          {t("issues.empty", { label: entry.trigger_label })}
        </p>
      ) : (
        <ul>
          {items.map((intake) => (
            <IntakeRow key={intake.id} intake={intake} />
          ))}
        </ul>
      )}
    </section>
  );
}
