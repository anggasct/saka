import { useT } from "@/i18n/use-t";
import { Badge } from "@/ui/badge";
import type { BadgeTone } from "@/ui/badge";

import type { WorkItem } from "./queries";
import { blockerText, label } from "./text";

function tone(state: string): BadgeTone {
  switch (state) {
    case "done":
      return "success";
    case "blocked":
      return "danger";
    case "in_stage":
    case "admitted":
      return "progress";
    default:
      return "neutral";
  }
}

export function TaskRow({ task }: { task: WorkItem }) {
  const t = useT();
  const place =
    task.stage && task.stage_phase
      ? t("task.place", {
          stage: label(t, "stage", task.stage),
          phase: label(t, "phase", task.stage_phase),
        })
      : task.stage
        ? label(t, "stage", task.stage)
        : null;
  const blocker = task.blocker;
  return (
    <li className="flex flex-col gap-2 border-t border-border p-4 first:border-t-0 sm:flex-row sm:items-start sm:gap-4">
      <div className="flex shrink-0 items-center gap-2 sm:w-40 sm:flex-col sm:items-start">
        <span className="text-sm font-semibold">
          {t("task.step", { step: task.step_id ?? task.task_key })}
        </span>
        <Badge tone={tone(task.state)}>
          {label(t, "task.state", task.state)}
        </Badge>
      </div>
      <div className="min-w-0 flex-1 space-y-1">
        <p className="break-words">{task.work}</p>
        {place !== null && (
          <p className="text-sm text-muted-foreground">{place}</p>
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
                blocker.params as Record<string, unknown>,
              )}
            </span>
            <code className="rounded bg-danger px-1 text-xs">
              {blocker.code}
            </code>
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
