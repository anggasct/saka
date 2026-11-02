import { useQueryClient } from "@tanstack/react-query";
import { useId, useState } from "react";

import { QUERY_ROOTS } from "@/api/events";
import { newIdempotencyKey } from "@/api/client";
import { useT } from "@/i18n/use-t";
import { Button } from "@/ui/button";

import { codeOf } from "./page-error";
import type { Session } from "./queries";
import { interruptSession, steerSession, stopSession } from "./queries";
import { ReasonDialog } from "./reason-dialog";
import { errorText } from "./text";

export const STEER_MAX = 4000;

export function SessionControls({ session }: { session: Session }) {
  const t = useT();
  const queryClient = useQueryClient();
  const [steer, setSteer] = useState("");
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const fieldId = useId();
  const countId = useId();
  const actions = session.available_actions;
  const canSteer = actions.includes("steer");
  const canInterrupt = actions.includes("interrupt");
  const canStop = actions.includes("stop");

  const refresh = async () => {
    await Promise.all([
      queryClient.invalidateQueries({ queryKey: [QUERY_ROOTS.sessions] }),
      queryClient.invalidateQueries({ queryKey: [QUERY_ROOTS.workItems] }),
    ]);
  };

  const run = async (action: () => Promise<void>) => {
    setPending(true);
    setError(null);
    try {
      await action();
      await refresh();
      return true;
    } catch (e) {
      setError(e);
      if (codeOf(e) === "invalid_state") {
        await refresh();
      }
      return false;
    } finally {
      setPending(false);
    }
  };

  const sendSteer = async () => {
    const text = steer.trim();
    if (await run(() => steerSession(session.id, text, newIdempotencyKey()))) {
      setSteer("");
    }
  };

  if (!canSteer && !canInterrupt && !canStop) {
    return null;
  }
  const steerBlocked =
    pending || steer.trim() === "" || steer.length > STEER_MAX;

  return (
    <div className="space-y-3">
      {canSteer && (
        <form
          className="space-y-1"
          onSubmit={(event) => {
            event.preventDefault();
            if (!steerBlocked) {
              void sendSteer();
            }
          }}
        >
          <label htmlFor={fieldId} className="text-sm font-medium">
            {t("session.steer.label")}
          </label>
          <textarea
            id={fieldId}
            value={steer}
            rows={3}
            aria-describedby={countId}
            aria-invalid={steer.length > STEER_MAX}
            onChange={(event) => {
              setSteer(event.target.value);
            }}
            onKeyDown={(event) => {
              if (
                event.key === "Enter" &&
                (event.metaKey || event.ctrlKey) &&
                !steerBlocked
              ) {
                event.preventDefault();
                void sendSteer();
              }
            }}
            className="w-full rounded-md border border-border bg-background p-2 text-sm"
          />
          <div className="flex flex-wrap items-center justify-between gap-2">
            <p id={countId} className="text-xs text-muted-foreground">
              {t("dialog.count", { count: steer.length, max: STEER_MAX })}
            </p>
            <Button type="submit" disabled={steerBlocked}>
              {t("session.steer.send")}
            </Button>
          </div>
        </form>
      )}
      <div className="flex flex-wrap gap-2">
        {canInterrupt && (
          <Button
            variant="outline"
            disabled={pending}
            onClick={() => {
              void run(() => interruptSession(session.id, newIdempotencyKey()));
            }}
          >
            {t("session.interrupt")}
          </Button>
        )}
        {canStop && (
          <ReasonDialog
            trigger={t("session.stop.trigger")}
            title={t("session.stop.title")}
            description={t("session.stop.description")}
            reasonLabel={t("dialog.reason")}
            submit={t("session.stop.submit")}
            required
            onSubmit={async (reason, key) => {
              try {
                await stopSession(session.id, reason ?? "", key);
              } catch (e) {
                if (codeOf(e) === "invalid_state") {
                  await refresh();
                }
                throw e;
              }
              await refresh();
            }}
          />
        )}
      </div>
      {error !== null && (
        <p role="alert" className="text-sm text-danger-foreground">
          {errorText(t, codeOf(error))}
        </p>
      )}
    </div>
  );
}
