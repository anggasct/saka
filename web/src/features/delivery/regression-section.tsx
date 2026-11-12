import { useT } from "@/i18n/use-t";
import { Badge } from "@/ui/badge";
import type { BadgeTone } from "@/ui/badge";

import type { WorkItem } from "./queries";
import { Section } from "./section";
import { sha } from "./text";

const MAX_CHECKS = 3;

function stateTone(state: string): BadgeTone {
  switch (state) {
    case "reproduced":
      return "success";
    case "failed":
      return "danger";
    default:
      return "progress";
  }
}

function resultTone(result: string): BadgeTone {
  return result === "passed" ? "neutral" : "attention";
}

export function RegressionSection({ task }: { task: WorkItem }) {
  const t = useT();
  const evidence = task.regression;
  if (evidence === null || evidence === undefined) {
    return null;
  }
  return (
    <Section title={t("task.regression.title")}>
      <p className="flex flex-wrap items-center gap-2">
        <Badge tone={stateTone(evidence.state)}>
          {t("task.regression.state", { state: evidence.state })}
        </Badge>
        <span className="text-sm text-muted-foreground">
          {t("task.regression.attempts", {
            attempts: evidence.attempts,
            max: MAX_CHECKS,
          })}
        </span>
        {evidence.commit_sha !== null && evidence.commit_sha !== undefined && (
          <span className="text-sm text-muted-foreground">
            {t("task.regression.commit", { sha: sha(evidence.commit_sha) })}
          </span>
        )}
      </p>
      {evidence.last_outcome !== null &&
        evidence.last_outcome !== undefined && (
          <p className="text-sm">
            {t("task.regression.outcome", { outcome: evidence.last_outcome })}
          </p>
        )}
      {evidence.last_outcome === "invalid_paths" &&
        evidence.paths.length > 0 && (
          <ul className="space-y-1 text-sm">
            {evidence.paths.map((path) => (
              <li key={path}>
                <code className="break-all">{path}</code>
              </li>
            ))}
          </ul>
        )}
      {evidence.commands.length > 0 && (
        <ul className="space-y-1 text-sm">
          {evidence.commands.map((command) => (
            <li
              key={command.command}
              className="flex flex-wrap items-center gap-2"
            >
              <code className="break-all">{command.command}</code>
              <Badge tone={resultTone(command.result)}>
                {t("task.regression.result", { result: command.result })}
              </Badge>
            </li>
          ))}
        </ul>
      )}
    </Section>
  );
}
