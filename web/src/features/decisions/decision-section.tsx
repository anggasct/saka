import { useId } from "react";

import { When } from "@/features/delivery/when";
import { Section } from "@/features/delivery/section";
import { label } from "@/features/delivery/text";
import { useT } from "@/i18n/use-t";
import { Badge } from "@/ui/badge";

import { DecisionForm } from "./decision-form";
import type { DecisionRequest, DecisionRun } from "./queries";
import { useRunDecisions } from "./queries";

function decisionAnchor(id: string): string {
  return `decision-${id}`;
}

function Answer({ decision }: { decision: DecisionRequest }) {
  const t = useT();
  const answer = decision.answer;
  if (answer === null || answer === undefined) {
    return null;
  }
  const chosen =
    answer.option === null || answer.option === undefined
      ? undefined
      : decision.options[answer.option];
  return (
    <dl className="space-y-1">
      <div>
        <dt className="font-medium">{t("decision.answer.chosen")}</dt>
        <dd className="break-words">
          {chosen === undefined
            ? t("decision.answer.noOption")
            : t("decision.answer.option", {
                number: (answer.option ?? 0) + 1,
                label: chosen.label,
              })}
        </dd>
      </div>
      {answer.note !== null && answer.note !== undefined && (
        <div>
          <dt className="font-medium">{t("decision.answer.note")}</dt>
          <dd className="break-words whitespace-pre-wrap">{answer.note}</dd>
        </div>
      )}
    </dl>
  );
}

function Entry({ decision }: { decision: DecisionRequest }) {
  const t = useT();
  const titleId = useId();
  const open = decision.state === "asked";
  return (
    <li>
      <article
        id={decisionAnchor(decision.id)}
        aria-labelledby={titleId}
        className="scroll-mt-4 space-y-2 rounded-md border border-border bg-card p-3 text-sm"
      >
        <div className="flex flex-wrap items-center justify-between gap-2">
          <h3 id={titleId} className="font-semibold break-words">
            {decision.question}
          </h3>
          <Badge tone={open ? "attention" : "neutral"}>
            {label(t, "decision.state", decision.state)}
          </Badge>
        </div>
        {decision.context !== null && decision.context !== undefined && (
          <p className="break-words whitespace-pre-wrap text-muted-foreground">
            {decision.context}
          </p>
        )}
        {open ? (
          <>
            <p className="text-xs text-muted-foreground">
              {t("decision.expires")} <When iso={decision.expires_at} />
            </p>
            <DecisionForm id={decision.id} options={decision.options} />
          </>
        ) : (
          <Answer decision={decision} />
        )}
      </article>
    </li>
  );
}

export function DecisionSection({ run }: { run: DecisionRun }) {
  const t = useT();
  const decisions = useRunDecisions(run);
  const items = decisions.data ?? [];
  if (items.length === 0) {
    return null;
  }
  return (
    <Section title={t("decision.section")}>
      <ul className="space-y-2">
        {items.map((decision) => (
          <Entry key={decision.id} decision={decision} />
        ))}
      </ul>
    </Section>
  );
}
