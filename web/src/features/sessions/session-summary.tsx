import { useFormat } from "@/features/delivery/format";
import { label, record } from "@/features/delivery/text";
import { When } from "@/features/delivery/when";
import { useT } from "@/i18n/use-t";

import type { Session } from "./queries";

function endText(t: ReturnType<typeof useT>, session: Session): string | null {
  const reason = session.end_reason;
  if (!reason) {
    return null;
  }
  const params = record(reason.params);
  const named = label(t, "session.end", reason.code);
  return typeof params.reason === "string" && params.reason !== ""
    ? t("session.endWithReason", { end: named, reason: params.reason })
    : named;
}

export function SessionSummary({ session }: { session: Session }) {
  const t = useT();
  const format = useFormat();
  const notReported = t("session.notReported");
  const ended = endText(t, session);
  return (
    <dl className="grid grid-cols-1 gap-x-4 gap-y-1 text-sm sm:grid-cols-[max-content_1fr]">
      <dt className="font-medium">{t("task.session.state")}</dt>
      <dd>{label(t, "session.state", session.state)}</dd>
      <dt className="font-medium">{t("task.session.cli")}</dt>
      <dd className="break-words">
        {session.cli_version
          ? t("session.cliVersion", {
              cli: session.cli,
              version: session.cli_version,
            })
          : session.cli}
      </dd>
      <dt className="font-medium">{t("session.model")}</dt>
      <dd className="break-words">{session.model ?? notReported}</dd>
      <dt className="font-medium">{t("session.turns")}</dt>
      <dd>{format.number(session.turns)}</dd>
      <dt className="font-medium">{t("session.running")}</dt>
      <dd>{format.duration(session.running_ms)}</dd>
      <dt className="font-medium">{t("session.cost")}</dt>
      <dd>
        {session.cost_usd
          ? t("session.costValue", { cost: session.cost_usd })
          : notReported}
      </dd>
      <dt className="font-medium">{t("session.tokens")}</dt>
      <dd>
        {session.input_tokens != null && session.output_tokens != null
          ? t("session.tokensValue", {
              input: format.number(session.input_tokens),
              output: format.number(session.output_tokens),
            })
          : notReported}
      </dd>
      {session.started_at && (
        <>
          <dt className="font-medium">{t("task.session.started")}</dt>
          <dd>
            <When iso={session.started_at} />
          </dd>
        </>
      )}
      {ended !== null && (
        <>
          <dt className="font-medium">{t("task.session.ended")}</dt>
          <dd className="break-words">{ended}</dd>
        </>
      )}
    </dl>
  );
}
