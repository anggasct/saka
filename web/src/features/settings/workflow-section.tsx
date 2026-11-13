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

type IssueEntry = NonNullable<Project["workflow"]["issue"]>;

function IssueDetails({ entry }: { entry: IssueEntry }) {
  const t = useT();
  const kinds = Object.entries(entry.kind_labels);
  return (
    <div className="space-y-2">
      <h3 className="text-sm font-medium">
        {t("settings.workflow.issue.title")}
      </h3>
      <dl className="grid gap-x-4 gap-y-1 rounded-lg border border-border p-3 text-sm sm:grid-cols-[max-content_1fr]">
        <dt className="text-muted-foreground">
          {t("settings.workflow.issue.triggerLabel")}
        </dt>
        <dd>
          <code className="rounded bg-muted px-1 text-xs break-all">
            {entry.trigger_label}
          </code>
        </dd>
        <dt className="text-muted-foreground">
          {t("settings.workflow.issue.labels")}
        </dt>
        <dd>
          <Patterns items={entry.labels} />
        </dd>
        <dt className="text-muted-foreground">
          {t("settings.workflow.issue.exclude")}
        </dt>
        <dd>
          <Patterns items={entry.exclude_labels} />
        </dd>
        <dt className="text-muted-foreground">
          {t("settings.workflow.issue.kindLabels")}
        </dt>
        <dd>
          {kinds.length === 0 ? (
            t("settings.workflow.any")
          ) : (
            <ul className="space-y-1">
              {kinds.map(([name, kind]) => (
                <li key={name}>
                  {t("settings.workflow.issue.kindLabel", {
                    name,
                    kind: label(t, "kind", kind),
                  })}
                </li>
              ))}
            </ul>
          )}
        </dd>
        <dt className="text-muted-foreground">
          {t("settings.workflow.issue.defaultKind")}
        </dt>
        <dd>{label(t, "kind", entry.default_kind)}</dd>
        <dt className="text-muted-foreground">
          {t("settings.workflow.issue.confidential")}
        </dt>
        <dd>
          {entry.include_confidential
            ? t("settings.workflow.included")
            : t("settings.workflow.skipped")}
        </dd>
        <dt className="text-muted-foreground">
          {t("settings.workflow.issue.maxOpen")}
        </dt>
        <dd>{entry.max_open}</dd>
        <dt className="text-muted-foreground">
          {t("settings.workflow.issue.bugfix")}
        </dt>
        <dd>
          {entry.bugfix === null || entry.bugfix === undefined ? (
            t("settings.workflow.issue.bugfix.none")
          ) : (
            <dl className="space-y-1">
              <dt className="text-muted-foreground">
                {t("settings.workflow.issue.doneWhen")}
              </dt>
              <dd>
                <Scope globs={entry.bugfix.done_when} />
              </dd>
              <dt className="text-muted-foreground">
                {t("settings.workflow.issue.regression")}
              </dt>
              <dd>
                <Scope globs={entry.bugfix.regression} />
              </dd>
            </dl>
          )}
        </dd>
      </dl>
    </div>
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
  const issue = workflow.issue;
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
            {issue !== null && issue !== undefined && (
              <li>
                {t("settings.workflow.entry.issue", {
                  trigger: label(
                    t,
                    "settings.workflow.issue.trigger",
                    issue.trigger,
                  ),
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
      {issue !== null && issue !== undefined && <IssueDetails entry={issue} />}
    </Section>
  );
}
