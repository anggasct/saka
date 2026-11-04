import { useT } from "@/i18n/use-t";

import { BudgetList } from "./budget-list";
import type { WorkItem } from "./queries";
import { Section } from "./section";
import { label, sha } from "./text";

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
          {env.docs && (
            <>
              <dt className="font-medium">{t("task.env.docs")}</dt>
              <dd className="flex flex-wrap gap-x-2 break-words">
                <code className="break-all">{env.docs.path}</code>
                <span>
                  {t("task.env.docsSource", {
                    folder:
                      env.docs.project_path === "."
                        ? env.docs.full_name
                        : `${env.docs.full_name}/${env.docs.project_path}`,
                    commit: sha(env.docs.commit),
                    mode: env.docs.mode,
                    branch: env.docs.branch ?? "",
                  })}
                </span>
              </dd>
            </>
          )}
        </dl>
      )}
      {budget && <BudgetList title={t("task.budget.title")} budget={budget} />}
    </Section>
  );
}
