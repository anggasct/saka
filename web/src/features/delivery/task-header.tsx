import { Link } from "@tanstack/react-router";
import type { ReactNode } from "react";

import { useT } from "@/i18n/use-t";
import { Badge } from "@/ui/badge";

import { ExternalBadges } from "./external-group";
import { permissionAnchor } from "@/features/sessions/permission-card";
import type { ProjectItem, SpecItem, WorkItem } from "./queries";
import { isRetired, placeText, stateTone } from "./task-state";
import { label } from "./text";
import { When } from "./when";

export function TaskHeader({
  task,
  spec,
  project,
  firstPermission,
  actions,
}: {
  task: WorkItem;
  spec: SpecItem | undefined;
  project: ProjectItem | undefined;
  firstPermission: string | undefined;
  actions: ReactNode;
}) {
  const t = useT();
  const place = placeText(t, task);
  const waiting = t("task.pendingPermissions", {
    count: task.pending_permission_count,
  });
  return (
    <header className="space-y-3">
      <Link
        to="/delivery"
        className="inline-flex min-h-11 items-center text-sm font-medium text-primary underline underline-offset-4"
      >
        {t("task.back")}
      </Link>
      <div className="flex flex-wrap items-center gap-2">
        <h1 className="text-2xl font-semibold break-all">
          {task.external || task.origin === "revert"
            ? task.task_key
            : t("task.step", { step: task.step_id ?? task.task_key })}
        </h1>
        <Badge tone={stateTone(task.state)}>
          {label(t, "task.state", task.state)}
        </Badge>
        <ExternalBadges task={task} />
        {task.origin === "revert" && <Badge>{t("revert.badge")}</Badge>}
        {task.pending_permission_count > 0 &&
          (firstPermission === undefined ? (
            <Badge tone="attention">{waiting}</Badge>
          ) : (
            <Link
              to="/delivery/tasks/$workItemId"
              params={{ workItemId: task.id }}
              hash={permissionAnchor(firstPermission)}
              className="inline-flex min-h-11 items-center rounded-full"
            >
              <Badge tone="attention">{waiting}</Badge>
            </Link>
          ))}
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
      {task.merge_auto === true && task.stage === "merge" && (
        <p className="text-sm text-muted-foreground">{t("task.mergesAuto")}</p>
      )}
      {task.state === "awaiting_start" && (
        <p className="text-sm text-muted-foreground">
          {t("task.awaitingStart")}
        </p>
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
        {task.autonomy !== null && task.autonomy !== undefined && (
          <>
            <dt className="font-medium">{t("task.autonomy")}</dt>
            <dd>{label(t, "autonomy", task.autonomy)}</dd>
          </>
        )}
        {task.external && (
          <>
            <dt className="font-medium">{t("external.author")}</dt>
            <dd className="break-words">{task.external.author}</dd>
          </>
        )}
        {task.revert_of && (
          <>
            <dt className="font-medium">{t("revert.of")}</dt>
            <dd className="break-all">
              <Link
                to="/delivery/tasks/$workItemId"
                params={{ workItemId: task.revert_of.id }}
                className="inline-flex min-h-11 items-center text-primary underline underline-offset-4"
              >
                {task.revert_of.key}
              </Link>
            </dd>
          </>
        )}
        {task.reverted_by && (
          <>
            <dt className="font-medium">{t("revert.by")}</dt>
            <dd className="flex flex-wrap items-center gap-2 break-all">
              <Link
                to="/delivery/tasks/$workItemId"
                params={{ workItemId: task.reverted_by.id }}
                className="inline-flex min-h-11 items-center text-primary underline underline-offset-4"
              >
                {task.reverted_by.key}
              </Link>
              <Badge tone={stateTone(task.reverted_by.state)}>
                {label(t, "task.state", task.reverted_by.state)}
              </Badge>
            </dd>
          </>
        )}
        {task.redo_of && (
          <>
            <dt className="font-medium">{t("redo.of")}</dt>
            <dd className="break-all">
              <Link
                to="/delivery/tasks/$workItemId"
                params={{ workItemId: task.redo_of.id }}
                className="inline-flex min-h-11 items-center text-primary underline underline-offset-4"
              >
                {task.redo_of.key}
              </Link>
            </dd>
          </>
        )}
        {task.redone_by && (
          <>
            <dt className="font-medium">{t("redo.by")}</dt>
            <dd className="flex flex-wrap items-center gap-2 break-all">
              <Link
                to="/delivery/tasks/$workItemId"
                params={{ workItemId: task.redone_by.id }}
                className="inline-flex min-h-11 items-center text-primary underline underline-offset-4"
              >
                {task.redone_by.key}
              </Link>
              <Badge tone={stateTone(task.redone_by.state)}>
                {label(t, "task.state", task.redone_by.state)}
              </Badge>
            </dd>
          </>
        )}
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
      {actions}
    </header>
  );
}
