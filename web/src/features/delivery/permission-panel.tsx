import { Link } from "@tanstack/react-router";
import { useId } from "react";

import { useT } from "@/i18n/use-t";

import { PermissionCard } from "@/features/sessions/permission-card";
import type { PermissionRequest, ProjectItem } from "./queries";
import { usePendingPermissions, useWorkItems } from "./queries";

const LINK =
  "inline-flex min-h-11 items-center font-medium text-primary underline underline-offset-4";

function Entry({
  request,
  project,
}: {
  request: PermissionRequest;
  project: ProjectItem | undefined;
}) {
  const t = useT();
  const tasks = useWorkItems(project?.id);
  const owner = request.owner;
  const workItemId = owner.kind === "stage_run" ? owner.work_item_id : null;
  const step = tasks.data?.find((w) => w.id === workItemId)?.step_id;
  return (
    <li>
      <PermissionCard
        request={request}
        project={project?.name}
        link={
          workItemId === null ? undefined : (
            <Link
              to="/delivery/tasks/$workItemId"
              params={{ workItemId }}
              className={LINK}
            >
              {step ? t("task.step", { step }) : t("permission.openTask")}
            </Link>
          )
        }
      />
    </li>
  );
}

export function PermissionPanel({
  projects,
}: {
  projects: readonly ProjectItem[];
}) {
  const t = useT();
  const pending = usePendingPermissions();
  const titleId = useId();
  const items = pending.data ?? [];
  if (items.length === 0) {
    return null;
  }
  return (
    <section aria-labelledby={titleId} className="space-y-2">
      <h2 id={titleId} className="text-lg font-semibold">
        {t("permission.panel", { count: items.length })}
      </h2>
      <ul className="space-y-2">
        {items.map((request) => (
          <Entry
            key={request.id}
            request={request}
            project={projects.find((p) => p.id === request.project_id)}
          />
        ))}
      </ul>
    </section>
  );
}
