import { Link } from "@tanstack/react-router";

import { useT } from "@/i18n/use-t";
import { Badge } from "@/ui/badge";

import type { WorkItem } from "./queries";
import { isRetired, placeText, stateTone } from "./task-state";
import { label } from "./text";

export function ExternalBadges({ task }: { task: WorkItem }) {
  const t = useT();
  const external = task.external;
  if (external === null || external === undefined) {
    return null;
  }
  return (
    <>
      {external.spec === null || external.spec === undefined ? (
        <Badge tone="attention">{t("external.noSpec")}</Badge>
      ) : (
        <Badge>
          {t("external.spec", {
            spec: external.spec.slug ?? external.spec.spec_id,
          })}
        </Badge>
      )}
      {external.from_fork && <Badge>{t("external.fork")}</Badge>}
      {external.draft && <Badge>{t("external.draft")}</Badge>}
    </>
  );
}

function ExternalRow({ task }: { task: WorkItem }) {
  const t = useT();
  const place = placeText(t, task);
  const external = task.external;
  return (
    <li
      className={`flex flex-col gap-2 border-t border-border p-4 first:border-t-0 sm:flex-row sm:items-start sm:gap-4 ${isRetired(task.state) ? "text-muted-foreground" : ""}`}
    >
      <div className="flex shrink-0 items-center gap-2 sm:w-40 sm:flex-col sm:items-start">
        <Link
          to="/delivery/tasks/$workItemId"
          params={{ workItemId: task.id }}
          className="inline-flex min-h-11 items-center text-sm font-semibold break-all text-primary underline underline-offset-4"
        >
          {task.task_key}
        </Link>
        <Badge tone={stateTone(task.state)}>
          {label(t, "task.state", task.state)}
        </Badge>
      </div>
      <div className="min-w-0 flex-1 space-y-1">
        <p className="break-words">{task.work}</p>
        <p className="flex flex-wrap items-center gap-2 text-sm text-muted-foreground">
          {external !== null && external !== undefined && (
            <span>{t("external.by", { author: external.author })}</span>
          )}
          {place !== null && <span>{place}</span>}
        </p>
        <div className="flex flex-wrap gap-2">
          <ExternalBadges task={task} />
        </div>
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

export function ExternalGroup({
  projectId,
  tasks,
}: {
  projectId: string;
  tasks: readonly WorkItem[];
}) {
  const t = useT();
  if (tasks.length === 0) {
    return null;
  }
  const headingId = `external-${projectId}`;
  return (
    <section aria-labelledby={headingId} className="border-t border-border">
      <header className="px-4 pt-4">
        <h3 id={headingId} className="font-medium break-words">
          {t("external.heading")}
        </h3>
      </header>
      <ul>
        {tasks.map((task) => (
          <ExternalRow key={task.id} task={task} />
        ))}
      </ul>
    </section>
  );
}
