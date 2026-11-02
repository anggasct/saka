import { useT } from "@/i18n/use-t";
import { Badge } from "@/ui/badge";
import type { BadgeTone } from "@/ui/badge";

import type { WorkItem } from "./queries";
import { Section } from "./section";
import { label, sha } from "./text";
import { When } from "./when";

function tone(outcome: string): BadgeTone {
  switch (outcome) {
    case "passed":
      return "success";
    case "failed":
      return "danger";
    default:
      return "progress";
  }
}

export function verifyingHead(task: WorkItem): string | null {
  if (task.stage !== "sync" || task.stage_phase !== "verifying") {
    return null;
  }
  const head = task.change_request?.head_sha;
  return task.gates.find((g) => g.head_sha !== head)?.head_sha ?? null;
}

export function GatesSection({ task }: { task: WorkItem }) {
  const t = useT();
  const target = verifyingHead(task);
  const title =
    target === null
      ? t("task.gates.title")
      : t("task.gates.verifying", { sha: sha(target) });
  return (
    <Section title={title}>
      {task.gates.length === 0 ? (
        <p className="text-sm text-muted-foreground">{t("task.gates.none")}</p>
      ) : (
        <ul className="space-y-4">
          {task.gates.map((gate) => (
            <li key={`${gate.source}:${gate.head_sha}`} className="space-y-2">
              <div className="flex flex-wrap items-center gap-2">
                <h3 className="font-medium">
                  {label(t, "gate.source", gate.source)}
                </h3>
                <Badge tone={tone(gate.outcome)}>
                  {label(t, "gate.outcome", gate.outcome)}
                </Badge>
                <span className="text-sm text-muted-foreground">
                  {gate.required
                    ? t("task.gates.required")
                    : t("task.gates.optional")}
                </span>
              </div>
              <p className="flex flex-wrap gap-x-3 text-sm text-muted-foreground">
                <span>{t("task.gates.at", { sha: sha(gate.head_sha) })}</span>
                <When iso={gate.observed_at} />
              </p>
              {gate.checks.length > 0 && (
                <ul className="space-y-1 text-sm">
                  {gate.checks.map((check) => (
                    <li
                      key={check.name}
                      className="flex flex-wrap items-center gap-x-2"
                    >
                      {check.url ? (
                        <a
                          className="inline-flex min-h-11 items-center break-all text-primary underline underline-offset-4"
                          href={check.url}
                          rel="noreferrer"
                          target="_blank"
                        >
                          {check.name}
                        </a>
                      ) : (
                        <code className="break-all">{check.name}</code>
                      )}
                      <span className="text-muted-foreground">
                        {check.conclusion
                          ? label(t, "check.conclusion", check.conclusion)
                          : label(t, "check.status", check.status)}
                      </span>
                    </li>
                  ))}
                </ul>
              )}
            </li>
          ))}
        </ul>
      )}
    </Section>
  );
}
