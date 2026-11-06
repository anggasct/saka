import { useT } from "@/i18n/use-t";

import type { WorkItem } from "./queries";
import { Section } from "./section";

export function TakenOverBanner({ task }: { task: WorkItem }) {
  const t = useT();
  if (task.state !== "taken_over") {
    return null;
  }
  return (
    <Section title={t("task.takenOver.title")}>
      <p className="text-sm">{t("task.takenOver.body")}</p>
      <p className="text-sm">
        <code className="break-all">{task.branch}</code>
      </p>
    </Section>
  );
}
