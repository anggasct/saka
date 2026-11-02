import { Link } from "@tanstack/react-router";

import { useT } from "@/i18n/use-t";
import { Badge } from "@/ui/badge";

import type { ProjectItem, SpecItem, WorkItem } from "./queries";
import { isRetired, placeText, stateTone } from "./task-state";
import { label } from "./text";
import { When } from "./when";

export function TaskHeader({
  task,
  spec,
  project,
}: {
  task: WorkItem;
  spec: SpecItem | undefined;
  project: ProjectItem | undefined;
}) {
  const t = useT();
  const place = placeText(t, task);
  return (
    <header className="space-y-3">
      <Link
        to="/delivery"
        className="inline-flex min-h-11 items-center text-sm font-medium text-primary underline underline-offset-4"
      >
        {t("task.back")}
      </Link>
      <div className="flex flex-wrap items-center gap-2">
        <h1 className="text-2xl font-semibold">
          {t("task.step", { step: task.step_id ?? task.task_key })}
        </h1>
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
      <p
        className={
          isRetired(task.state)
            ? "break-words text-muted-foreground"
            : "break-words"
        }
      >
        {task.work}
      </p>
      {place !== null && (
        <p className="text-sm text-muted-foreground">{place}</p>
      )}
      <dl className="grid grid-cols-1 gap-x-4 gap-y-1 text-sm sm:grid-cols-[max-content_1fr]">
        <dt className="font-medium">{t("task.key")}</dt>
        <dd className="break-all">
          <code>{task.task_key}</code>
        </dd>
        <dt className="font-medium">{t("task.branch")}</dt>
        <dd className="break-all">
          <code>{task.branch}</code>
        </dd>
        {spec !== undefined && (
          <>
            <dt className="font-medium">{t("task.spec")}</dt>
            <dd className="flex flex-wrap gap-x-3 break-words">
              <span>{spec.title}</span>
              <code className="break-all">{spec.path}</code>
            </dd>
          </>
        )}
        {project !== undefined && (
          <>
            <dt className="font-medium">{t("task.project")}</dt>
            <dd className="break-words">{project.name}</dd>
          </>
        )}
        {task.admitted_at && (
          <>
            <dt className="font-medium">{t("task.admitted")}</dt>
            <dd>
              <When iso={task.admitted_at} />
            </dd>
          </>
        )}
        {task.done_at && (
          <>
            <dt className="font-medium">{t("task.doneAt")}</dt>
            <dd>
              <When iso={task.done_at} />
            </dd>
          </>
        )}
      </dl>
    </header>
  );
}
