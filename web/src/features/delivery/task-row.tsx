import { Link } from "@tanstack/react-router";

import { GateBadges } from "@/features/deployments/deploy-section";
import { useT } from "@/i18n/use-t";
import { Badge } from "@/ui/badge";

import { useFormat } from "./format";
import type { WorkItem } from "./queries";
import { isRetired, placeText, stateTone } from "./task-state";
import { blockerText, holdsCount, label, record, waitingText } from "./text";

export function TaskRow({
  task,
  tasks,
}: {
  task: WorkItem;
  tasks: readonly WorkItem[];
}) {
  const t = useT();
  const format = useFormat();
  const place = placeText(t, task);
  const blocker = task.blocker;
  const waiting = task.state === "queued" ? task.waiting_on : [];
  const first = waiting[0];
  const holds = task.state === "blocked" ? holdsCount(task, tasks) : 0;
  return (
    <li
      className={`flex flex-col gap-2 border-t border-border p-4 first:border-t-0 sm:flex-row sm:items-start sm:gap-4 ${isRetired(task.state) ? "text-muted-foreground" : ""}`}
    >
      <div className="flex shrink-0 items-center gap-2 sm:w-40 sm:flex-col sm:items-start">
        <Link
          to="/delivery/tasks/$workItemId"
          params={{ workItemId: task.id }}
          className="inline-flex min-h-11 items-center text-sm font-semibold text-primary underline underline-offset-4"
        >
          {t("task.step", { step: task.step_id ?? task.task_key })}
        </Link>
        <Badge tone={stateTone(task.state)}>
          {label(t, "task.state", task.state)}
        </Badge>
        {task.pending_permission_count > 0 && (
          <Badge tone="attention">
            {t("task.pendingPermissions", {
              count: task.pending_permission_count,
            })}
          </Badge>
        )}
      </div>
      <div className="min-w-0 flex-1 space-y-1">
        <p className="break-words">{task.work}</p>
        {place !== null && (
          <p className="text-sm text-muted-foreground">{place}</p>
        )}
        {task.reverted_by && (
          <p className="flex flex-wrap items-center gap-2 text-sm text-muted-foreground">
            <Badge tone="attention">
              {task.reverted_by.state === "done"
                ? t("revert.reverted")
                : t("revert.reverting")}
            </Badge>
            <Link
              to="/delivery/tasks/$workItemId"
              params={{ workItemId: task.reverted_by.id }}
              className="inline-flex min-h-11 items-center break-all text-primary underline underline-offset-4"
            >
              {task.reverted_by.key}
            </Link>
          </p>
        )}
        {first !== undefined && (
          <p className="text-sm break-words text-muted-foreground">
            {waitingText(t, first, format.relative, format.percent)}
            {waiting.length > 1 && (
              <> {t("waiting.more", { count: waiting.length - 1 })}</>
            )}
          </p>
        )}
        {blocker && (
          <p
            className="flex flex-wrap items-center gap-x-2 text-sm text-danger-foreground"
            role="status"
          >
            <span>
              {blockerText(
                t,
                blocker.code,
                record(blocker.params),
                format.relative,
              )}
            </span>
            <code className="rounded bg-danger px-1 text-xs">
              {blocker.code}
            </code>
          </p>
        )}
        <GateBadges task={task} />
        {holds > 0 && (
          <p className="text-sm font-medium">
            {t("task.holds", { count: holds })}
          </p>
        )}
      </div>
      {task.change_request && (
        <a
          className="inline-flex min-h-11 min-w-11 shrink-0 items-center text-sm font-medium text-primary underline underline-offset-4"
          href={task.change_request.url}
          rel="noreferrer"
          target="_blank"
        >
          {t("task.change", { number: task.change_request.number })}
        </a>
      )}
    </li>
  );
}
