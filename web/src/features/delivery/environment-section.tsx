import { useT } from "@/i18n/use-t";
import { Badge } from "@/ui/badge";

import type { WorkItem } from "./queries";
import { Section } from "./section";
import { label, sha, text } from "./text";

const DIMENSIONS = ["running_minutes", "turns", "tokens", "cost_usd"] as const;

export function EnvironmentSection({ task }: { task: WorkItem }) {
  const t = useT();
  const env = task.environment;
  const budget = task.budget;
  if (!env && !budget) {
    return null;
  }
  return (
    <Section title={t("task.env.title")}>
      {env && (
        <dl className="grid grid-cols-1 gap-x-4 gap-y-1 text-sm sm:grid-cols-[max-content_1fr]">
          <dt className="font-medium">{t("task.env.state")}</dt>
          <dd>{label(t, "env.state", env.state)}</dd>
          <dt className="font-medium">{t("task.env.ports")}</dt>
          <dd>
            {env.ports
              ? t("task.env.portRange", {
                  start: String(env.ports.start),
                  end: String(env.ports.start + env.ports.count - 1),
                })
              : t("task.env.noPorts")}
          </dd>
          <dt className="font-medium">{t("task.env.toolchains")}</dt>
          <dd className="break-words">
            {env.toolchains.length > 0
              ? env.toolchains.map((c) => `${c.tool} ${c.version}`).join(", ")
              : t("task.env.noToolchains")}
          </dd>
          <dt className="font-medium">{t("task.env.recipe")}</dt>
          <dd>
            {env.recipe_sha256 ? (
              <code>{sha(env.recipe_sha256)}</code>
            ) : (
              t("task.env.noRecipe")
            )}
          </dd>
        </dl>
      )}
      {budget && (
        <div className="space-y-1">
          <h3 className="font-medium">{t("task.budget.title")}</h3>
          <dl className="grid grid-cols-1 gap-x-4 gap-y-1 text-sm sm:grid-cols-[max-content_1fr]">
            {DIMENSIONS.map((dimension) => {
              const measure = budget[dimension];
              const used = text(measure.used);
              const limit = Number(text(measure.limit));
              const near =
                limit > 0 &&
                (Number(used) / limit) * 100 >= budget.warn_percent;
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
      )}
    </Section>
  );
}
