import { useT } from "@/i18n/use-t";
import { Badge } from "@/ui/badge";

import type { Measure } from "./queries";
import { label, text } from "./text";

const DIMENSIONS = ["running_minutes", "turns", "tokens", "cost_usd"] as const;

export type BudgetUse = Record<(typeof DIMENSIONS)[number], Measure> & {
  warn_percent: number;
};

export function BudgetList({
  title,
  budget,
}: {
  title: string;
  budget: BudgetUse;
}) {
  const t = useT();
  return (
    <div className="space-y-1">
      <h3 className="font-medium">{title}</h3>
      <dl className="grid grid-cols-1 gap-x-4 gap-y-1 text-sm sm:grid-cols-[max-content_1fr]">
        {DIMENSIONS.map((dimension) => {
          const measure = budget[dimension];
          const used = text(measure.used);
          const limit = Number(text(measure.limit));
          const near =
            limit > 0 && (Number(used) / limit) * 100 >= budget.warn_percent;
          return (
            <div key={dimension} className="contents">
              <dt className="font-medium">
                {label(t, "budget.dimension", dimension)}
              </dt>
              <dd className="flex flex-wrap items-center gap-2">
                {limit > 0
                  ? t("task.budget.measure", {
                      used,
                      limit: text(measure.limit),
                    })
                  : t("task.budget.unlimited", { used })}
                {near && (
                  <Badge tone="attention">{t("task.budget.near")}</Badge>
                )}
              </dd>
            </div>
          );
        })}
      </dl>
    </div>
  );
}
