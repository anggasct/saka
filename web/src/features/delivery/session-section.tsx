import { useId, useState } from "react";

import { useT } from "@/i18n/use-t";
import { Skeleton } from "@/ui/skeleton";

import { useFormat } from "./format";
import type { PermissionRequest, Session, WorkItem } from "./queries";
import { useSessions } from "./queries";
import { Section } from "./section";
import { SessionControls } from "./session-controls";
import { label, record } from "./text";
import { Timeline } from "./timeline";
import { When } from "./when";

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

function Summary({ session }: { session: Session }) {
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

export function SessionSection({
  task,
  project,
  permissions,
}: {
  task: WorkItem;
  project: string | undefined;
  permissions: readonly PermissionRequest[];
}) {
  const t = useT();
  const format = useFormat();
  const sessions = useSessions(task.id);
  const [chosen, setChosen] = useState<string | null>(null);
  const pickerId = useId();
  const list = sessions.data ?? [];
  const session = list.find((s) => s.id === chosen) ?? list.at(-1);
  return (
    <Section title={t("task.session.title")}>
      {sessions.isPending ? (
        <Skeleton className="h-24" />
      ) : sessions.isError ? (
        <p className="text-sm text-muted-foreground">{t("session.error")}</p>
      ) : session === undefined ? (
        <p className="text-sm text-muted-foreground">
          {t("task.session.none")}
        </p>
      ) : (
        <>
          {list.length > 1 && (
            <div className="space-y-1">
              <label htmlFor={pickerId} className="text-sm font-medium">
                {t("session.picker")}
              </label>
              <select
                id={pickerId}
                value={session.id}
                onChange={(event) => {
                  setChosen(event.target.value);
                }}
                className="block min-h-11 w-full rounded-md border border-border bg-background px-2 text-sm"
              >
                {list.map((s, i) => (
                  <option key={s.id} value={s.id}>
                    {t("session.option", {
                      number: i + 1,
                      started: s.started_at
                        ? format.relative(s.started_at)
                        : t("session.notStarted"),
                      state: label(t, "session.state", s.state),
                      resumed: s.resumed_from_session_id ? "yes" : "no",
                    })}
                  </option>
                ))}
              </select>
            </div>
          )}
          <Summary session={session} />
          <SessionControls key={`controls-${session.id}`} session={session} />
          <Timeline
            key={`timeline-${session.id}`}
            sessionId={session.id}
            project={project}
            permissions={permissions}
          />
        </>
      )}
    </Section>
  );
}
