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

type KindOverride = Project["workflow"]["kinds"][string];

function KindOverrides({ kinds }: { kinds: Project["workflow"]["kinds"] }) {
  const t = useT();
  const entries = Object.entries(kinds);
  if (entries.length === 0) {
    return (
      <p className="text-sm text-muted-foreground">
        {t("settings.workflow.kinds.none")}
      </p>
    );
  }
  return (
    <ul aria-label={t("settings.workflow.kinds.title")} className="space-y-2">
      {entries.map(([kind, override]) => (
        <KindEntry key={kind} kind={kind} override={override} />
      ))}
    </ul>
  );
}

function KindEntry({
  kind,
  override,
}: {
  kind: string;
  override: KindOverride;
}) {
  const t = useT();
  const stages = Object.entries(override.stages);
  return (
    <li className="space-y-2 rounded-lg border border-border p-3 text-sm">
      <span className="font-medium">{label(t, "kind", kind)}</span>
      <dl className="grid gap-x-4 gap-y-1 sm:grid-cols-[max-content_1fr]">
        <dt className="text-muted-foreground">
          {t("settings.workflow.stages")}
        </dt>
        <dd>
          {stages.length === 0 ? (
            t("settings.workflow.kinds.noStages")
          ) : (
            <ul className="space-y-1">
              {stages.map(([stage, trigger]) => (
                <li key={stage}>
                  {t("settings.workflow.stage", {
                    stage: label(t, "stage", stage),
                    trigger: label(t, "trigger", trigger),
                  })}
                </li>
              ))}
            </ul>
          )}
        </dd>
        <dt className="text-muted-foreground">
          {t("settings.workflow.kinds.autonomy")}
        </dt>
        <dd>
          {override.autonomy === null || override.autonomy === undefined
            ? t("settings.workflow.kinds.noAutonomy")
            : label(t, "autonomy", override.autonomy)}
        </dd>
        {override.test_paths.length > 0 && (
          <>
            <dt className="text-muted-foreground">
              {t("settings.workflow.kinds.testPaths")}
            </dt>
            <dd>
              <Scope globs={override.test_paths} />
            </dd>
          </>
        )}
      </dl>
    </li>
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
        <dt className="text-muted-foreground">
          {t("settings.workflow.unreviewedChore")}
        </dt>
        <dd>
          {project.allow_unreviewed_chore
            ? t("settings.workflow.unreviewedChore.allowed")
            : t("settings.workflow.unreviewedChore.refused")}
        </dd>
      </dl>
      <div className="space-y-2">
        <h3 className="text-sm font-medium">
          {t("settings.workflow.kinds.title")}
        </h3>
        <KindOverrides kinds={workflow.kinds} />
      </div>
      {entry !== null && entry !== undefined && <ChangeFilters entry={entry} />}
    </Section>
  );
}
