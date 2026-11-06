import { useT } from "@/i18n/use-t";

import type { WorkItem } from "./queries";
import { Section } from "./section";

export function MergeQueueSection({ task }: { task: WorkItem }) {
  const t = useT();
  const queue = task.merge_queue;
  if (!queue) {
    return null;
  }
  return (
    <Section title={t("task.queue.title")}>
      <p className="text-sm break-words">
        {queue.position === 0
          ? t("task.queue.turn", { branch: queue.base_branch })
          : t("task.queue.waiting", {
              branch: queue.base_branch,
              ahead: queue.position - 1,
              size: queue.size,
            })}
      </p>
    </Section>
  );
}
