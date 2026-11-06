import { useQueryClient } from "@tanstack/react-query";
import { useId, useState } from "react";

import { QUERY_ROOTS } from "@/api/events";
import { useT } from "@/i18n/use-t";

import { ActionDialog } from "./action-dialog";
import { useFormat } from "./format";
import { codeOf } from "./page-error";
import type { WorkItem, WorkItemDetail } from "./queries";
import {
  cancelTask,
  mergeTask,
  requestFix,
  requestReview,
  retryTask,
  returnTask,
  runPrimaryAction,
  startTask,
  takeoverTask,
} from "./queries";
import { blockerText, label, record, sha } from "./text";

const RELOAD_ON = [
  "version_stale",
  "invalid_state",
  "blocker_not_open",
  "blocker_action_changed",
];

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

function StartDialog({ task }: { task: WorkItem }) {
  const t = useT();
  const after = useAfterAction();
  return (
    <ActionDialog
      trigger={t("task.start.trigger")}
      triggerVariant="default"
      title={t("task.start.title")}
      description={t("task.start.description")}
      submit={t("task.start.submit")}
      onSubmit={(_, key) => after(() => startTask(task.id, task.version, key))}
    />
  );
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

export function TakeoverDialog({
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
      trigger={t("task.takeover.trigger")}
      triggerVariant={primary ? "default" : "outline"}
      title={t("task.takeover.title")}
      description={t("task.takeover.description")}
      reason={{ label: t("dialog.reason"), required: true }}
      submit={t("task.takeover.submit")}
      onSubmit={(reason, key) =>
        after(() =>
          primary && run
            ? run(reason ?? "", key)
            : takeoverTask(task.id, reason ?? "", key),
        )
      }
    />
  );
}

function ReturnDialog({ task }: { task: WorkItem | WorkItemDetail }) {
  const t = useT();
  const after = useAfterAction();
  const stageId = useId();
  const runs = "stage_runs" in task ? task.stage_runs : [];
  const left = runs.at(-1)?.stage;
  const canMerge =
    task.change_request !== null && task.change_request !== undefined;
  const canFix =
    canMerge &&
    task.review?.outcome === "changes_requested" &&
    task.review.current;
  const [stage, setStage] = useState(
    canMerge && left === "merge" ? "merge" : "dev",
  );
  return (
    <ActionDialog
      trigger={t("task.return.trigger")}
      triggerVariant="default"
      title={t("task.return.title")}
      description={t("task.return.description")}
      reason={{ label: t("dialog.reason"), required: true }}
      submit={t("task.return.submit")}
      onSubmit={(reason, key) =>
        after(() => returnTask(task.id, stage, reason ?? "", key))
      }
    >
      <div className="space-y-1">
        <label htmlFor={stageId} className="text-sm font-medium">
          {t("task.return.stage")}
        </label>
        <select
          id={stageId}
          value={stage}
          onChange={(event) => {
            setStage(event.target.value);
          }}
          className="block min-h-11 w-full rounded-md border border-border bg-background px-2 text-sm"
        >
          <option value="dev">{label(t, "stage", "dev")}</option>
          {canMerge && (
            <option value="review">{label(t, "stage", "review")}</option>
          )}
          {canFix && <option value="fix">{label(t, "stage", "fix")}</option>}
          {canMerge && (
            <option value="merge">{label(t, "stage", "merge")}</option>
          )}
        </select>
      </div>
    </ActionDialog>
  );
}

function ReviewDialog({ task }: { task: WorkItem }) {
  const t = useT();
  const after = useAfterAction();
  const change = task.change_request;
  return (
    <ActionDialog
      trigger={t("task.review.trigger")}
      triggerVariant="default"
      title={t("task.review.title")}
      description={t("task.review.description")}
      submit={t("task.review.submit")}
      onSubmit={(_, key) => after(() => requestReview(task.id, key))}
    >
      {change && (
        <dl className="grid grid-cols-1 gap-x-4 gap-y-1 text-sm sm:grid-cols-[max-content_1fr]">
          <dt className="font-medium">{t("task.change.title")}</dt>
          <dd>{t("task.change", { number: change.number })}</dd>
          <dt className="font-medium">{t("task.head")}</dt>
          <dd>
            <code>{sha(change.head_sha)}</code>
          </dd>
        </dl>
      )}
    </ActionDialog>
  );
}

function FixDialog({ task }: { task: WorkItem }) {
  const t = useT();
  const after = useAfterAction();
  const change = task.change_request;
  return (
    <ActionDialog
      trigger={t("task.fixAction.trigger")}
      triggerVariant="default"
      title={t("task.fixAction.title")}
      description={t("task.fixAction.description")}
      submit={t("task.fixAction.submit")}
      onSubmit={(_, key) => after(() => requestFix(task.id, key))}
    >
      {change && (
        <dl className="grid grid-cols-1 gap-x-4 gap-y-1 text-sm sm:grid-cols-[max-content_1fr]">
          <dt className="font-medium">{t("task.change.title")}</dt>
          <dd>{t("task.change", { number: change.number })}</dd>
          <dt className="font-medium">{t("task.head")}</dt>
          <dd>
            <code>{sha(change.head_sha)}</code>
          </dd>
        </dl>
      )}
    </ActionDialog>
  );
}

export function TaskActions({ task }: { task: WorkItem | WorkItemDetail }) {
  const primary = task.blocker?.primary_action.kind;
  const actions = task.available_actions.filter((a) => a !== primary);
  if (actions.length === 0) {
    return null;
  }
  return (
    <div className="flex flex-wrap gap-2">
      {actions.includes("start") && <StartDialog task={task} />}
      {actions.includes("review") && <ReviewDialog task={task} />}
      {actions.includes("fix") && <FixDialog task={task} />}
      {actions.includes("merge") && <MergeDialog task={task} />}
      {actions.includes("retry") && <RetryDialog task={task} />}
      {actions.includes("return") && <ReturnDialog task={task} />}
      {actions.includes("takeover") && <TakeoverDialog task={task} />}
      {actions.includes("cancel") && <CancelDialog task={task} />}
    </div>
  );
}
