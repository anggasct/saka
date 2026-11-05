import { label } from "@/features/delivery/text";
import { Section } from "@/features/delivery/section";
import { useT } from "@/i18n/use-t";

import type { Project } from "./queries";
import { Scope } from "./repos-section";

type Entry = NonNullable<Project["workflow"]["change_request"]>;

function Patterns({ items }: { items: readonly string[] }) {
  const t = useT();
  return items.length === 0 ? (
    <span>{t("settings.workflow.any")}</span>
  ) : (
    <Scope globs={items} />
  );
}

function ChangeFilters({ entry }: { entry: Entry }) {
  const t = useT();
  const included = (on: boolean) =>
    on ? t("settings.workflow.included") : t("settings.workflow.skipped");
  return (
    <dl
      aria-label={t("settings.workflow.filters")}
      className="grid gap-x-4 gap-y-1 rounded-lg border border-border p-3 text-sm sm:grid-cols-[max-content_1fr]"
    >
      <dt className="text-muted-foreground">{t("settings.workflow.labels")}</dt>
      <dd>
        <Patterns items={entry.labels} />
      </dd>
      <dt className="text-muted-foreground">
        {t("settings.workflow.authors")}
      </dt>
      <dd>
        <Patterns items={entry.authors} />
      </dd>
      <dt className="text-muted-foreground">
        {t("settings.workflow.branches")}
      </dt>
      <dd>
        <Patterns items={entry.branches} />
      </dd>
      <dt className="text-muted-foreground">{t("settings.workflow.paths")}</dt>
      <dd>
        <Patterns items={entry.paths} />
      </dd>
      <dt className="text-muted-foreground">{t("settings.workflow.forks")}</dt>
      <dd>{included(entry.include_forks)}</dd>
      <dt className="text-muted-foreground">{t("settings.workflow.drafts")}</dt>
      <dd>{included(entry.include_drafts)}</dd>
      <dt className="text-muted-foreground">
        {t("settings.workflow.triggerLabel")}
      </dt>
      <dd>
        <code className="rounded bg-muted px-1 text-xs break-all">
          {entry.trigger_label}
        </code>
      </dd>
    </dl>
  );
}

export function WorkflowSection({ project }: { project: Project }) {
  const t = useT();
  const { workflow } = project;
  const entry = workflow.change_request;
  return (
    <Section title={t("settings.workflow.title")}>
      <dl className="grid gap-x-4 gap-y-2 text-sm sm:grid-cols-[max-content_1fr]">
        <dt className="text-muted-foreground">
          {t("settings.workflow.preset")}
        </dt>
        <dd>{label(t, "workflowPreset", workflow.preset)}</dd>
        <dt className="text-muted-foreground">
          {t("settings.workflow.autonomy")}
        </dt>
        <dd>{label(t, "autonomy", project.autonomy)}</dd>
        <dt className="text-muted-foreground">
          {t("settings.workflow.entries")}
        </dt>
        <dd>
          <ul className="space-y-1">
            {workflow.spec && <li>{t("settings.workflow.entry.spec")}</li>}
            {entry !== null && entry !== undefined && (
              <li>
                {t("settings.workflow.entry.change_request", {
                  trigger: label(t, "trigger", entry.trigger),
                })}
              </li>
            )}
          </ul>
        </dd>
        <dt className="text-muted-foreground">
          {t("settings.workflow.stages")}
        </dt>
        <dd>
          {workflow.stages.length === 0 ? (
            t("settings.workflow.noStages")
          ) : (
            <ul className="space-y-1">
              {workflow.stages.map((s) => (
                <li key={s.stage}>
                  {t("settings.workflow.stage", {
                    stage: label(t, "stage", s.stage),
                    trigger: label(t, "trigger", s.trigger),
                  })}
                </li>
              ))}
            </ul>
          )}
        </dd>
      </dl>
      {entry !== null && entry !== undefined && <ChangeFilters entry={entry} />}
    </Section>
  );
}
