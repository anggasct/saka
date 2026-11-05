import { useQueryClient } from "@tanstack/react-query";

import { QUERY_ROOTS } from "@/api/events";
import { useT } from "@/i18n/use-t";

import { ActionDialog } from "./action-dialog";
import { useFormat } from "./format";
import { codeOf } from "./page-error";
import type { WorkItem } from "./queries";
import { cancelTask, mergeTask, retryTask, runPrimaryAction } from "./queries";
import { blockerText, label, record, sha } from "./text";

const RELOAD_ON = ["version_stale", "invalid_state", "blocker_not_open"];

function useAfterAction() {
  const queryClient = useQueryClient();
  const refresh = () =>
    Promise.all([
      queryClient.invalidateQueries({ queryKey: [QUERY_ROOTS.workItems] }),
      queryClient.invalidateQueries({ queryKey: [QUERY_ROOTS.sessions] }),
    ]);
  return async (action: () => Promise<void>) => {
    try {
      await action();
    } catch (e) {
      if (RELOAD_ON.includes(codeOf(e))) {
        await refresh();
      }
      throw e;
    }
    await refresh();
  };
}

function MergeDialog({ task }: { task: WorkItem }) {
  const t = useT();
  const after = useAfterAction();
  const change = task.change_request;
  return (
    <ActionDialog
      trigger={t("task.merge.trigger")}
      triggerVariant="default"
      title={t("task.merge.title")}
      description={t("task.merge.description")}
      reason={{ label: t("dialog.reasonOptional"), required: false }}
      submit={t("task.merge.submit")}
      onSubmit={(reason, key) =>
        after(() => mergeTask(task.id, task.version, reason, key))
      }
    >
      <dl className="grid grid-cols-1 gap-x-4 gap-y-1 text-sm sm:grid-cols-[max-content_1fr]">
        {change && (
          <>
            <dt className="font-medium">{t("task.change.title")}</dt>
            <dd>{t("task.change", { number: change.number })}</dd>
            <dt className="font-medium">{t("task.head")}</dt>
            <dd>
              <code>{sha(change.head_sha)}</code>
            </dd>
          </>
        )}
        {task.gates.map((gate) => (
          <div key={gate.source} className="contents">
            <dt className="font-medium">
              {label(t, "gate.source", gate.source)}
            </dt>
            <dd>{label(t, "gate.outcome", gate.outcome)}</dd>
          </div>
        ))}
      </dl>
    </ActionDialog>
  );
}

type Run = (reason: string, idempotencyKey: string) => Promise<void>;

function primaryRun(task: WorkItem): Run | undefined {
  const blocker = task.blocker;
  if (!blocker) {
    return undefined;
  }
  return (reason, key) => runPrimaryAction(task.id, blocker.id, reason, key);
}

export function RetryDialog({
  task,
  primary = false,
}: {
  task: WorkItem;
  primary?: boolean;
}) {
  const t = useT();
  const format = useFormat();
  const after = useAfterAction();
  const blocker = task.blocker;
  const run = primaryRun(task);
  return (
    <ActionDialog
      trigger={t("task.retry.trigger")}
      triggerVariant={primary ? "default" : "outline"}
      title={t("task.retry.title")}
      description={t("task.retry.description")}
      reason={{ label: t("dialog.reason"), required: true }}
      submit={t("task.retry.submit")}
      onSubmit={(reason, key) =>
        after(() =>
          primary && run
            ? run(reason ?? "", key)
            : retryTask(task.id, reason ?? "", key),
        )
      }
    >
      {blocker && (
        <dl className="grid grid-cols-1 gap-x-4 gap-y-1 text-sm sm:grid-cols-[max-content_1fr]">
          <dt className="font-medium">{t("task.blocker.title")}</dt>
          <dd className="break-words">
            {blockerText(
              t,
              blocker.code,
              record(blocker.params),
              format.relative,
            )}
          </dd>
          {blocker.stage && (
            <>
              <dt className="font-medium">{t("task.blocker.stage")}</dt>
              <dd>{label(t, "stage", blocker.stage)}</dd>
            </>
          )}
        </dl>
      )}
    </ActionDialog>
  );
}

export function CancelDialog({
  task,
  primary = false,
}: {
  task: WorkItem;
  primary?: boolean;
}) {
  const t = useT();
  const after = useAfterAction();
  const run = primaryRun(task);
  return (
    <ActionDialog
      trigger={t("task.cancel.trigger")}
      triggerVariant={primary ? "default" : "outline"}
      title={t("task.cancel.title")}
      description={t("task.cancel.description")}
      reason={{ label: t("dialog.reason"), required: true }}
      submit={t("task.cancel.submit")}
      onSubmit={(reason, key) =>
        after(() =>
          primary && run
            ? run(reason ?? "", key)
            : cancelTask(task.id, reason ?? "", key),
        )
      }
    />
  );
}

export function TaskActions({ task }: { task: WorkItem }) {
  const primary = task.blocker?.primary_action.kind;
  const actions = task.available_actions.filter((a) => a !== primary);
  if (actions.length === 0) {
    return null;
  }
  return (
    <div className="flex flex-wrap gap-2">
      {actions.includes("merge") && <MergeDialog task={task} />}
      {actions.includes("retry") && <RetryDialog task={task} />}
      {actions.includes("cancel") && <CancelDialog task={task} />}
    </div>
  );
}
