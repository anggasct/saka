import { Link } from "@tanstack/react-router";

import { useT } from "@/i18n/use-t";

import { useFormat } from "./format";
import type { WorkItem } from "./queries";
import { Section } from "./section";
import { holdsCount, waitingText } from "./text";

const LINK =
  "inline-flex min-h-11 items-center font-medium text-primary underline underline-offset-4";

export function WaitingSection({
  task,
  tasks,
}: {
  task: WorkItem;
  tasks: readonly WorkItem[];
}) {
  const t = useT();
  const format = useFormat();
  const waiting = task.state === "queued" ? task.waiting_on : [];
  const holds = holdsCount(task, tasks);
  if (waiting.length === 0 && holds === 0) {
    return null;
  }
  return (
    <Section title={t("task.waiting.title")}>
      {waiting.length > 0 && (
        <ul className="space-y-1 text-sm">
          {waiting.map((item) => {
            const sentence = waitingText(
              t,
              item,
              format.relative,
              format.percent,
            );
            const key =
              item.kind === "step"
                ? item.work_item_id
                : item.kind === "spec"
                  ? item.spec_id
                  : item.kind === "quota"
                    ? `${item.account}:${item.window}`
                    : item.kind;
            return (
              <li key={key} className="break-words">
                {item.kind === "step" ? (
                  <Link
                    to="/delivery/tasks/$workItemId"
                    params={{ workItemId: item.work_item_id }}
                    className={LINK}
                  >
                    {sentence}
                  </Link>
                ) : (
                  sentence
                )}
              </li>
            );
          })}
        </ul>
      )}
      {holds > 0 && (
        <p className="text-sm font-medium">
          {t("task.holds", { count: holds })}
        </p>
      )}
    </Section>
  );
}
