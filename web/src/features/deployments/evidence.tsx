import { useFormat } from "@/features/delivery/format";
import { label } from "@/features/delivery/text";
import { When } from "@/features/delivery/when";
import { useT } from "@/i18n/use-t";
import { Badge } from "@/ui/badge";

import type { DeploymentCheck } from "./queries";
import { checkTone } from "./text";

const PHASES = ["deploy", "ready", "verify"] as const;

function Check({ check }: { check: DeploymentCheck }) {
  const t = useT();
  const format = useFormat();
  const name = check.command ?? t("deploy.evidence.action");
  return (
    <li className="space-y-1 text-sm">
      <p className="flex flex-wrap items-center gap-x-2 gap-y-1">
        {check.command === null || check.command === undefined ? (
          <span className="font-medium">{name}</span>
        ) : (
          <code className="font-medium break-all">{name}</code>
        )}
        <Badge tone={checkTone(check.state)}>
          {label(t, "deploy.check", check.state)}
        </Badge>
        {check.exit_code !== null && check.exit_code !== undefined && (
          <span className="text-muted-foreground">
            {t("deploy.evidence.exit", { code: check.exit_code })}
          </span>
        )}
        {check.http_status !== null && check.http_status !== undefined && (
          <span className="text-muted-foreground">
            {t("deploy.evidence.http", { status: check.http_status })}
          </span>
        )}
        <span className="text-muted-foreground">
          {t("deploy.evidence.attempts", { count: check.attempts })}
        </span>
        <span className="text-muted-foreground">
          {format.duration(check.duration_ms)}
        </span>
        <span className="text-xs text-muted-foreground">
          <When iso={check.finished_at} />
        </span>
      </p>
      {check.output_tail !== "" && (
        <pre
          role="log"
          tabIndex={0}
          aria-label={t("deploy.evidence.output", { name })}
          className="max-h-64 overflow-auto rounded bg-muted p-2 text-xs break-words whitespace-pre-wrap"
        >
          {check.output_tail}
        </pre>
      )}
    </li>
  );
}

export function Evidence({ checks }: { checks: readonly DeploymentCheck[] }) {
  const t = useT();
  const phases = PHASES.map((phase) => ({
    phase,
    items: checks
      .filter((c) => c.phase === phase)
      .toSorted((a, b) => a.ordinal - b.ordinal),
  })).filter((p) => p.items.length > 0);
  if (phases.length === 0) {
    return (
      <p className="text-sm text-muted-foreground">
        {t("deploy.evidence.none")}
      </p>
    );
  }
  return (
    <div className="space-y-3">
      {phases.map(({ phase, items }) => (
        <section key={phase} className="space-y-2">
          <h4 className="font-medium">{label(t, "deploy.phase", phase)}</h4>
          <ol className="space-y-3">
            {items.map((check) => (
              <Check key={`${phase}:${check.ordinal}`} check={check} />
            ))}
          </ol>
        </section>
      ))}
    </div>
  );
}
