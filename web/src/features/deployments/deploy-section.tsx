import { Link } from "@tanstack/react-router";

import { Section } from "@/features/delivery/section";
import type { WorkItem } from "@/features/delivery/queries";
import { label, sha } from "@/features/delivery/text";
import { useT } from "@/i18n/use-t";
import { Badge } from "@/ui/badge";

import { gateReason, gateTone, stateTone } from "./text";

type Gate = WorkItem["deploy_gates"][number];

function GateDeployment({
  gate,
  projectId,
}: {
  gate: Gate;
  projectId: string;
}) {
  const t = useT();
  const deployment = gate.deployment;
  if (deployment === null || deployment === undefined) {
    return (
      <p className="text-sm text-muted-foreground">
        {t("task.deploy.pending")}
      </p>
    );
  }
  return (
    <p className="flex flex-wrap items-center gap-2 text-sm">
      <Badge tone={stateTone(deployment.state)}>
        {label(t, "deploy.state", deployment.state)}
      </Badge>
      {deployment.phase !== null && deployment.phase !== undefined && (
        <span className="text-muted-foreground">
          {label(t, "deploy.phase", deployment.phase)}
        </span>
      )}
      {deployment.sha !== null && deployment.sha !== undefined && (
        <code>{sha(deployment.sha)}</code>
      )}
      <Link
        to="/deployments/$projectId"
        params={{ projectId }}
        search={{ environment: gate.environment, deployment: deployment.id }}
        className="inline-flex min-h-11 items-center text-primary underline underline-offset-4"
      >
        {t("task.deploy.open")}
      </Link>
    </p>
  );
}

export function DeploySection({ task }: { task: WorkItem }) {
  const t = useT();
  if (task.deploy_gates.length === 0) {
    return null;
  }
  return (
    <Section title={t("task.deploy.title")}>
      <ul className="space-y-4">
        {task.deploy_gates.map((gate) => (
          <li key={gate.environment} className="space-y-1">
            <div className="flex flex-wrap items-center gap-2">
              <h3 className="font-medium break-words">{gate.environment}</h3>
              <Badge tone={gateTone(gate.state)}>
                {label(t, "deploy.gate", gate.state)}
              </Badge>
            </div>
            <GateDeployment gate={gate} projectId={task.project_id} />
            {gate.reason !== null && gate.reason !== undefined && (
              <p className="text-sm break-words text-muted-foreground">
                {t("task.deploy.reason", {
                  reason: gateReason(t, gate.state, gate.reason),
                })}
              </p>
            )}
          </li>
        ))}
      </ul>
    </Section>
  );
}

export function GateBadges({ task }: { task: WorkItem }) {
  const t = useT();
  if (task.deploy_gates.length === 0) {
    return null;
  }
  return (
    <ul className="flex flex-wrap gap-1">
      {task.deploy_gates.map((gate) => (
        <li key={gate.environment}>
          <Badge tone={gateTone(gate.state)}>
            {t("task.deploy.badge", {
              environment: gate.environment,
              state: label(t, "deploy.gate", gate.state),
            })}
          </Badge>
        </li>
      ))}
    </ul>
  );
}
