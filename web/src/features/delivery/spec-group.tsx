import { useT } from "@/i18n/use-t";
import { Badge } from "@/ui/badge";

import type { SpecItem, WorkItem } from "./queries";
import { SpecPausedBanner } from "./spec-paused-banner";
import { TaskRow } from "./task-row";
import { label } from "./text";

export function SpecGroup({
  spec,
  tasks,
  projectTasks,
}: {
  spec: SpecItem;
  tasks: readonly WorkItem[];
  projectTasks: readonly WorkItem[];
}) {
  const t = useT();
  const ordered = [...tasks].sort(
    (a, b) =>
      (a.step_index ?? 0) - (b.step_index ?? 0) ||
      a.created_at.localeCompare(b.created_at),
  );
  const headingId = `spec-${spec.id}`;
  return (
    <section aria-labelledby={headingId} className="border-t border-border">
      <header className="flex flex-wrap items-center gap-2 px-4 pt-4">
        <h3 id={headingId} className="font-medium break-words">
          {spec.title}
        </h3>
        <Badge tone={spec.state === "shipped" ? "success" : "neutral"}>
          {label(t, "spec.state", spec.state)}
        </Badge>
      </header>
      <SpecPausedBanner spec={spec} tasks={ordered} />
      {ordered.length === 0 ? (
        <p className="p-4 text-sm text-muted-foreground">
          {t("delivery.noTasks")}
        </p>
      ) : (
        <ul>
          {ordered.map((task) => (
            <TaskRow key={task.id} task={task} tasks={projectTasks} />
          ))}
        </ul>
      )}
    </section>
  );
}
