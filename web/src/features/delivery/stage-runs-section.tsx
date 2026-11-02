import { useT } from "@/i18n/use-t";
import { Badge } from "@/ui/badge";

import { useFormat } from "./format";
import type { SpecItem, WorkItem, WorkItemDetail } from "./queries";
import { Section } from "./section";
import { blockerText, label, record, sha } from "./text";
import { When } from "./when";

export function advanceDelay(
  task: WorkItem,
  spec: SpecItem | undefined,
  tasks: readonly WorkItem[],
): { after: string; ms: number } | null {
  const admitted = task.admitted_at;
  const step = spec?.steps.find((s) => s.id === task.step_id);
  if (!admitted || step === undefined) {
    return null;
  }
  let latest: { after: string; merged: number } | null = null;
  for (const dependency of step.after) {
    const merged = tasks.find(
      (o) => o.spec_id === task.spec_id && o.step_id === dependency,
    )?.change_request?.merged_at;
    const at = merged ? Date.parse(merged) : Number.NaN;
    if (!Number.isNaN(at) && (latest === null || at > latest.merged)) {
      latest = { after: dependency, merged: at };
    }
  }
  return latest === null
    ? null
    : { after: latest.after, ms: Date.parse(admitted) - latest.merged };
}

export function StageRunsSection({
  task,
  spec,
  tasks,
}: {
  task: WorkItemDetail;
  spec: SpecItem | undefined;
  tasks: readonly WorkItem[];
}) {
  const t = useT();
  const format = useFormat();
  const delay = advanceDelay(task, spec, tasks);
  return (
    <Section title={t("task.runs.title")}>
      {delay !== null && (
        <p className="text-sm">
          {t("task.runs.advance", {
            duration: format.duration(delay.ms),
            step: delay.after,
          })}
        </p>
      )}
      {task.stage_runs.length === 0 ? (
        <p className="text-sm text-muted-foreground">{t("task.runs.none")}</p>
      ) : (
        <ol className="divide-y divide-border">
          {task.stage_runs.map((run) => {
            const started = run.started_at;
            const ended = run.ended_at;
            const ms = started
              ? (ended ? Date.parse(ended) : format.now) - Date.parse(started)
              : null;
            return (
              <li key={run.id} className="space-y-1 py-3 text-sm">
                <div className="flex flex-wrap items-center gap-2">
                  <h3 className="font-medium">
                    {t("task.runs.attempt", {
                      stage: label(t, "stage", run.stage),
                      attempt: run.attempt,
                    })}
                  </h3>
                  <Badge>{label(t, "run.state", run.state)}</Badge>
                  <span className="text-muted-foreground">
                    {label(t, "run.trigger", run.trigger)}
                  </span>
                </div>
                {run.reason && <p className="break-words">{run.reason}</p>}
                {run.failure && (
                  <p className="break-words text-danger-foreground">
                    {blockerText(
                      t,
                      run.failure.code,
                      record(run.failure.params),
                      format.relative,
                    )}
                  </p>
                )}
                <p className="flex flex-wrap gap-x-3 text-muted-foreground">
                  {run.head_sha && (
                    <span>
                      {t("task.gates.at", { sha: sha(run.head_sha) })}
                    </span>
                  )}
                  {started && <When iso={started} />}
                  {ms !== null && <span>{format.duration(ms)}</span>}
                </p>
              </li>
            );
          })}
        </ol>
      )}
    </Section>
  );
}
