import { Link, Navigate } from "@tanstack/react-router";
import { useId, useState } from "react";

import { ApiError } from "@/api/client";
import { fieldParams } from "@/features/delivery/action-dialog";
import { PageError } from "@/features/delivery/page-error";
import { errorText } from "@/features/delivery/text";
import { useT } from "@/i18n/use-t";
import { platform } from "@/platform";
import { Button } from "@/ui/button";
import { Card } from "@/ui/card";
import { Input } from "@/ui/input";
import { Skeleton } from "@/ui/skeleton";

import { GoTo } from "./go-to";
import { nextSearch, safeNext } from "./next";
import { registerPasskey, useAuthStatus } from "./queries";
import type { PasskeyOutcome } from "./queries";

const SETUP_COMMAND = "saka auth setup";

export function SetupPage({ next }: { next: string | undefined }) {
  const t = useT();
  const status = useAuthStatus();
  const target = safeNext(next);
  const codeId = useId();
  const codeHintId = useId();
  const nameId = useId();
  const nameHintId = useId();
  const [code, setCode] = useState("");
  const [name, setName] = useState("");
  const [pending, setPending] = useState(false);
  const [message, setMessage] = useState<string | null>(null);

  const submit = async () => {
    setPending(true);
    setMessage(null);
    try {
      const outcome: PasskeyOutcome = await registerPasskey(
        name.trim(),
        code.trim(),
      );
      if (outcome === "ok") {
        platform.navigateTo(target);
        return;
      }
      setMessage(
        outcome === "unsupported"
          ? t("auth.setup.unsupported")
          : outcome === "cancelled"
            ? t("auth.setup.cancelled")
            : outcome === "duplicate"
              ? t("auth.setup.duplicate")
              : t("auth.setup.failed"),
      );
    } catch (error) {
      setMessage(
        error instanceof ApiError
          ? errorText(t, error.code, fieldParams(error.params))
          : t("auth.setup.failed"),
      );
    }
    setPending(false);
  };

  if (status.error !== null) {
    return (
      <div className="mx-auto w-full max-w-md p-4">
        <PageError
          title="auth.setup.error"
          error={status.error}
          onRetry={() => void status.refetch()}
        />
      </div>
    );
  }
  if (status.isPending) {
    return (
      <div
        role="status"
        aria-busy="true"
        aria-label={t("auth.login.loading")}
        className="mx-auto w-full max-w-md p-4"
      >
        <Skeleton className="h-48" />
      </div>
    );
  }
  if (!status.data.enabled) {
    return <Navigate to="/" replace />;
  }
  if (status.data.authenticated) {
    return <GoTo path={target} />;
  }
  const ready = code.trim() !== "" && name.trim() !== "" && !pending;
  return (
    <div className="mx-auto w-full max-w-md space-y-4 p-4">
      <Card className="space-y-4 p-4">
        <h1 className="text-2xl font-semibold">{t("auth.setup.title")}</h1>
        <p className="text-sm text-muted-foreground">
          {t(
            status.data.setup_required
              ? "auth.setup.first"
              : "auth.setup.again",
            {
              command: SETUP_COMMAND,
            },
          )}
        </p>
        <form
          className="space-y-3"
          onSubmit={(event) => {
            event.preventDefault();
            if (ready) {
              void submit();
            }
          }}
        >
          <div className="space-y-1">
            <label htmlFor={codeId} className="text-sm font-medium">
              {t("auth.setup.code")}
            </label>
            <Input
              id={codeId}
              value={code}
              onChange={(event) => {
                setCode(event.target.value);
              }}
              autoComplete="off"
              autoCapitalize="characters"
              spellCheck={false}
              aria-describedby={codeHintId}
              className="font-mono tracking-widest uppercase"
            />
            <p id={codeHintId} className="text-xs text-muted-foreground">
              {t("auth.setup.codeHint")}
            </p>
          </div>
          <div className="space-y-1">
            <label htmlFor={nameId} className="text-sm font-medium">
              {t("auth.setup.name")}
            </label>
            <Input
              id={nameId}
              value={name}
              onChange={(event) => {
                setName(event.target.value);
              }}
              autoComplete="off"
              maxLength={100}
              aria-describedby={nameHintId}
            />
            <p id={nameHintId} className="text-xs text-muted-foreground">
              {t("auth.setup.nameHint")}
            </p>
          </div>
          <Button type="submit" disabled={!ready}>
            {pending ? t("auth.setup.pending") : t("auth.setup.submit")}
          </Button>
        </form>
        {message !== null && (
          <p role="alert" className="text-sm text-danger-foreground">
            {message}
          </p>
        )}
        {!status.data.setup_required && (
          <Link
            to="/login"
            search={nextSearch(next)}
            className="inline-flex min-h-11 items-center text-sm font-medium text-primary underline underline-offset-4"
          >
            {t("auth.setup.signInLink")}
          </Link>
        )}
      </Card>
    </div>
  );
}
