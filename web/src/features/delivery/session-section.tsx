import { useId, useState } from "react";

import { SessionControls } from "@/features/sessions/session-controls";
import { SessionSummary } from "@/features/sessions/session-summary";
import { Timeline } from "@/features/sessions/timeline";
import { useSessions } from "@/features/sessions/queries";
import type { SessionsOf } from "@/features/sessions/queries";
import { useT } from "@/i18n/use-t";
import { Skeleton } from "@/ui/skeleton";

import { useFormat } from "./format";
import type { PermissionRequest } from "./queries";
import { Section } from "./section";
import { label } from "./text";

export function SessionSection({
  owner,
  attemptId,
  title,
  empty,
  project,
  permissions,
}: {
  owner: SessionsOf;
  attemptId?: string | null;
  title: string;
  empty: string;
  project: string | undefined;
  permissions: readonly PermissionRequest[];
}) {
  const format = useFormat();
  const t = useT();
  const sessions = useSessions(owner);
  const [chosen, setChosen] = useState<string | null>(null);
  const pickerId = useId();
  const list = (sessions.data ?? []).filter(
    (s) =>
      attemptId === undefined ||
      (s.owner.kind === "draft_attempt" && s.owner.attempt_id === attemptId),
  );
  const session = list.find((s) => s.id === chosen) ?? list.at(-1);
  return (
    <Section title={title}>
      {sessions.isPending ? (
        <Skeleton className="h-24" />
      ) : sessions.isError ? (
        <p className="text-sm text-muted-foreground">{t("session.error")}</p>
      ) : session === undefined ? (
        <p className="text-sm text-muted-foreground">{empty}</p>
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
          <SessionSummary session={session} />
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
