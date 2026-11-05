import { useQueryClient } from "@tanstack/react-query";
import { useEffect, useRef, useState } from "react";

import { QUERY_ROOTS } from "@/api/events";
import { useFormat } from "@/features/delivery/format";
import { label, text } from "@/features/delivery/text";
import { useT } from "@/i18n/use-t";
import { platform } from "@/platform";
import { Button } from "@/ui/button";

import { invalidate, useAuthorization } from "./queries";
import type { Authorization } from "./queries";

const SECOND_MS = 1000;
type Copy = "idle" | "copied" | "failed";

function useSecondClock(): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const id = setInterval(() => {
      setNow(Date.now());
    }, SECOND_MS);
    return () => {
      clearInterval(id);
    };
  }, []);
  return now;
}

export function DevicePanel({
  initial,
  onCompleted,
  onRetry,
  retrying,
}: {
  initial: Authorization;
  onCompleted: (login: string) => void;
  onRetry: () => void;
  retrying: boolean;
}) {
  const t = useT();
  const client = useQueryClient();
  const format = useFormat();
  const now = useSecondClock();
  const [copy, setCopy] = useState<Copy>("idle");
  const closed = useRef(false);
  const query = useAuthorization(initial.integration_id, initial, (current) =>
    current.state === "pending" ? current.interval_secs * SECOND_MS : false,
  );
  const auth = query.data;

  useEffect(() => {
    if (auth.state !== "completed" || closed.current) {
      return;
    }
    closed.current = true;
    void Promise.all([
      invalidate(client, QUERY_ROOTS.userLinks),
      invalidate(client, QUERY_ROOTS.authorship),
    ]);
    onCompleted(auth.link?.login ?? "");
  }, [auth, client, onCompleted]);

  const copyCode = async () => {
    const ok = await platform.copyText(auth.user_code);
    setCopy(ok ? "copied" : "failed");
  };

  if (auth.state === "completed") {
    return null;
  }
  if (auth.state !== "pending") {
    const failure = auth.failure;
    const sentence =
      auth.state === "failed"
        ? t("settings.link.failed", {
            code: label(t, "settings.link.failure", text(failure?.code)),
          })
        : t(
            auth.state === "expired"
              ? "settings.link.expired"
              : "settings.link.denied",
          );
    return (
      <div role="alert" className="space-y-2 text-sm text-danger-foreground">
        <p>{sentence}</p>
        <Button variant="outline" disabled={retrying} onClick={onRetry}>
          {t("settings.link.retry")}
        </Button>
      </div>
    );
  }

  const left = Date.parse(auth.expires_at) - now;
  return (
    <div className="space-y-3 rounded-md border border-border p-3">
      <p className="text-sm">{t("settings.link.instructions")}</p>
      <p className="font-mono text-2xl tracking-widest select-all">
        {auth.user_code}
      </p>
      <div className="flex flex-wrap items-center gap-3">
        <Button variant="outline" onClick={() => void copyCode()}>
          {t("settings.link.copy")}
        </Button>
        <a
          href={auth.verification_uri}
          target="_blank"
          rel="noopener noreferrer"
          className="inline-flex min-h-11 items-center text-sm font-medium text-primary underline underline-offset-4"
        >
          {t("settings.link.open", { uri: auth.verification_uri })}
        </a>
      </div>
      <p role="status" className="text-sm">
        {copy === "copied"
          ? t("settings.link.copied")
          : copy === "failed"
            ? t("settings.link.copyFailed")
            : ""}
      </p>
      <p className="text-sm">{t("settings.link.waiting")}</p>
      <p className="text-sm text-muted-foreground">
        {t("settings.link.expires", {
          time: format.duration(Math.max(0, left)),
        })}
      </p>
    </div>
  );
}
