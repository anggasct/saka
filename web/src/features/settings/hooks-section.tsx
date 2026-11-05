import { useQueryClient } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";

import { QUERY_ROOTS } from "@/api/events";
import { ActionDialog } from "@/features/delivery/action-dialog";
import { PageError } from "@/features/delivery/page-error";
import { Section } from "@/features/delivery/section";
import { label, record, text } from "@/features/delivery/text";
import { When } from "@/features/delivery/when";
import { useT } from "@/i18n/use-t";
import { Badge } from "@/ui/badge";
import type { BadgeTone } from "@/ui/badge";
import { Skeleton } from "@/ui/skeleton";

import { hookDetails, hookErrorText, hookSummary } from "./hook-text";
import { rerunHook, useHookRuns, useHooks } from "./queries";
import type { HookRun } from "./queries";

const TONES: Record<string, BadgeTone> = {
  pending: "progress",
  succeeded: "success",
  failed: "danger",
};

function Loading({ name }: { name: string }) {
  return (
    <div role="status" aria-busy="true" aria-label={name} className="space-y-2">
      <Skeleton className="h-10" />
      <Skeleton className="h-10" />
    </div>
  );
}

function Definitions({ projectId }: { projectId: string }) {
  const t = useT();
  const hooks = useHooks(projectId);
  if (hooks.error !== null) {
    return (
      <PageError
        title="settings.hooks.error"
        error={hooks.error}
        onRetry={() => void hooks.refetch()}
      />
    );
  }
  if (hooks.isPending) {
    return <Loading name={t("settings.hooks.loading")} />;
  }
  if (hooks.data.length === 0) {
    return (
      <p className="text-sm text-muted-foreground">
        {t("settings.hooks.empty")}
      </p>
    );
  }
  return (
    <ul className="divide-y divide-border">
      {hooks.data.map((hook) => (
        <li key={hook.name} className="space-y-1 py-2">
          <p className="flex min-w-0 flex-wrap items-baseline gap-2">
            <code className="font-medium break-all">{hook.name}</code>
            <span className="text-sm text-muted-foreground">
              {hookSummary(t, hook)}
            </span>
          </p>
          <dl className="grid gap-x-4 gap-y-1 text-sm sm:grid-cols-[max-content_1fr]">
            {hookDetails(t, hook).map((row) => (
              <div key={row.key} className="contents">
                <dt className="text-muted-foreground">{row.name}</dt>
                <dd className="break-all">{row.value}</dd>
              </div>
            ))}
            <dt className="text-muted-foreground">
              {t("settings.hooks.detail.timeout")}
            </dt>
            <dd>{t("settings.hooks.seconds", { count: hook.timeout_secs })}</dd>
          </dl>
        </li>
      ))}
    </ul>
  );
}

function RerunDialog({ run, projectId }: { run: HookRun; projectId: string }) {
  const t = useT();
  const client = useQueryClient();
  return (
    <ActionDialog
      trigger={t("settings.hooks.rerun", { hook: run.hook })}
      title={t("settings.hooks.rerunTitle", { hook: run.hook })}
      description={t("settings.hooks.rerunBody")}
      reason={{ label: t("dialog.reason"), required: true }}
      submit={t("settings.hooks.rerunSubmit")}
      onSubmit={async (reason, key) => {
        await rerunHook(run.id, reason ?? "", key);
        await client.invalidateQueries({
          queryKey: [QUERY_ROOTS.hookRuns, projectId],
        });
      }}
    />
  );
}

function RunRow({ run, projectId }: { run: HookRun; projectId: string }) {
  const t = useT();
  const gate = run.mode === "gate";
  const tail = text(record(run.result).output_tail);
  return (
    <li className="space-y-1 py-2">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <p className="flex min-w-0 flex-wrap items-center gap-2">
          <Badge tone={TONES[run.state] ?? "neutral"}>
            {label(t, "hook.state", run.state)}
          </Badge>
          <code className="font-medium break-all">{run.hook}</code>
          <span className="text-sm text-muted-foreground">
            {t("settings.hooks.runLine", {
              event: label(t, "hook.event", run.trigger),
              action: label(t, "hook.action", run.action),
              mode: label(t, "hook.mode", run.mode),
            })}
          </span>
        </p>
        {!gate && run.state !== "pending" && (
          <RerunDialog run={run} projectId={projectId} />
        )}
      </div>
      <p className="flex flex-wrap gap-x-2 text-xs text-muted-foreground">
        <When iso={run.queued_at} />
        {run.work_item_id !== null && run.work_item_id !== undefined && (
          <Link
            to="/delivery/tasks/$workItemId"
            params={{ workItemId: run.work_item_id }}
            className="text-primary underline underline-offset-4"
          >
            {run.task_key}
          </Link>
        )}
        {run.rerun_of !== null && run.rerun_of !== undefined && (
          <span>{t("settings.hooks.isRerun")}</span>
        )}
      </p>
      {run.error !== null && run.error !== undefined && (
        <p className="text-sm break-words text-danger-foreground">
          {hookErrorText(t, run.error)}
        </p>
      )}
      {tail !== "" && (
        <pre className="rounded bg-muted p-2 text-xs break-words whitespace-pre-wrap">
          {tail}
        </pre>
      )}
    </li>
  );
}

function Runs({ projectId }: { projectId: string }) {
  const t = useT();
  const runs = useHookRuns(projectId);
  if (runs.error !== null) {
    return (
      <PageError
        title="settings.hooks.runsError"
        error={runs.error}
        onRetry={() => void runs.refetch()}
      />
    );
  }
  if (runs.isPending) {
    return <Loading name={t("settings.hooks.runsLoading")} />;
  }
  if (runs.data.length === 0) {
    return (
      <p className="text-sm text-muted-foreground">
        {t("settings.hooks.noRuns")}
      </p>
    );
  }
  return (
    <ul className="divide-y divide-border">
      {runs.data.map((run) => (
        <RunRow key={run.id} run={run} projectId={projectId} />
      ))}
    </ul>
  );
}

export function HooksSection({ projectId }: { projectId: string }) {
  const t = useT();
  return (
    <Section title={t("settings.hooks.title")}>
      <Definitions projectId={projectId} />
      <h3 className="text-base font-semibold">{t("settings.hooks.runs")}</h3>
      <Runs projectId={projectId} />
    </Section>
  );
}
