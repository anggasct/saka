import { Link } from "@tanstack/react-router";

import type { ChipTone } from "@/editor/chips";
import { label } from "@/features/delivery/text";
import { useT } from "@/i18n/use-t";
import { Card } from "@/ui/card";
import { Skeleton } from "@/ui/skeleton";
import type { LintIssue } from "@/wasm/spec-tools";

import type { FileWrite, Spec, WorkItem } from "./queries";
import { issueCounts } from "./spec-lint";
import { fileErrorText, shortSha } from "./text";

export type StepRow = {
  id: string;
  line: number;
  task: WorkItem | null;
  chip: { text: string; tone: ChipTone } | null;
};

const TONE: Record<ChipTone, string> = {
  done: "chip-done",
  blocked: "chip-blocked",
  active: "chip-active",
  waiting: "chip-waiting",
};

export function SpecPanel({
  spec,
  loading,
  localIssues,
  serverIssues,
  steps,
  problem,
}: {
  spec: Spec | null | undefined;
  loading: boolean;
  localIssues: readonly LintIssue[];
  serverIssues: readonly LintIssue[];
  steps: readonly StepRow[];
  problem: FileWrite["spec_problem"];
}) {
  const t = useT();
  const counts = issueCounts([...localIssues, ...serverIssues]);
  const anyTask = steps.some((s) => s.task !== null);
  return (
    <Card aria-labelledby="docs-spec" className="space-y-3 p-3">
      <h2 id="docs-spec" className="text-sm font-semibold">
        {t("docs.panel.title")}
      </h2>
      {problem && (
        <p role="alert" className="text-sm text-danger-foreground">
          {t("docs.panel.problem", {
            reason: fileErrorText(
              t,
              problem.code,
              typeof problem.params === "object" && problem.params !== null
                ? (problem.params as Record<string, unknown>)
                : {},
            ),
          })}
        </p>
      )}
      {loading && spec === undefined ? (
        <Skeleton className="h-16" />
      ) : (
        <dl className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-1 text-sm">
          {spec && (
            <>
              <dt className="sr-only">{t("docs.panel.title")}</dt>
              <dd className="col-span-2 font-medium">{spec.title}</dd>
            </>
          )}
          <dt className="text-muted-foreground">{t("docs.panel.state")}</dt>
          <dd>
            {spec
              ? label(t, "spec.state", spec.state)
              : t("docs.panel.notRegistered")}
          </dd>
          <dt className="text-muted-foreground">{t("docs.panel.checks")}</dt>
          <dd>
            {counts.errors + counts.warnings === 0
              ? t("docs.panel.ready")
              : t("docs.panel.issues", counts)}
          </dd>
          {spec && (
            <>
              <dt className="text-muted-foreground">
                {t("docs.panel.revision")}
              </dt>
              <dd className="font-mono">
                {shortSha(spec.revision.source_commit)}
              </dd>
            </>
          )}
        </dl>
      )}
      <section aria-labelledby="docs-delivery" className="space-y-1">
        <h3 id="docs-delivery" className="text-sm font-semibold">
          {t("docs.panel.delivery")}
        </h3>
        {!anyTask ? (
          <p className="text-sm text-muted-foreground">
            {t("docs.panel.notApproved")}
          </p>
        ) : (
          <ul className="space-y-1 text-sm">
            {steps.map((step) => (
              <li key={step.id} className="flex items-center gap-2">
                <span className="font-mono">{step.id}</span>
                {step.task && step.chip ? (
                  <Link
                    to="/delivery/tasks/$workItemId"
                    params={{ workItemId: step.task.id }}
                    className={`inline-flex min-h-11 items-center rounded-full px-2 text-xs underline-offset-2 hover:underline lg:min-h-8 ${TONE[step.chip.tone]}`}
                  >
                    {step.chip.text}
                  </Link>
                ) : (
                  <span className="text-muted-foreground">
                    {t("docs.panel.noTask")}
                  </span>
                )}
              </li>
            ))}
          </ul>
        )}
      </section>
    </Card>
  );
}
