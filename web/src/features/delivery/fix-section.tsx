import { useT } from "@/i18n/use-t";
import { Badge } from "@/ui/badge";

import type { WorkItemDetail } from "./queries";
import { Section } from "./section";

export function FixSection({ task }: { task: WorkItemDetail }) {
  const t = useT();
  const fix = task.fix;
  if (fix === null || fix === undefined) {
    return null;
  }
  return (
    <Section title={t("task.fix.title")}>
      <p className="flex flex-wrap items-center gap-2">
        <Badge>
          {t("task.fix.round", { round: fix.round, max: fix.max_rounds })}
        </Badge>
        <span>{t("task.fix.cause", { cause: fix.cause })}</span>
      </p>
      {fix.round > fix.max_rounds && (
        <p className="text-sm text-muted-foreground">{t("task.fix.beyond")}</p>
      )}
    </Section>
  );
}
