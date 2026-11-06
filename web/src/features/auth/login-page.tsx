import { Link, Navigate } from "@tanstack/react-router";
import { useState } from "react";

import { ApiError } from "@/api/client";
import { PageError } from "@/features/delivery/page-error";
import { errorText } from "@/features/delivery/text";
import { useT } from "@/i18n/use-t";
import { platform } from "@/platform";
import { Button } from "@/ui/button";
import { Card } from "@/ui/card";
import { Skeleton } from "@/ui/skeleton";

import { GoTo } from "./go-to";
import { nextSearch, safeNext } from "./next";
import { signInWithPasskey, useAuthStatus } from "./queries";

type Phase = "idle" | "pending";

export function LoginPage({ next }: { next: string | undefined }) {
  const t = useT();
  const status = useAuthStatus();
  const target = safeNext(next);
  const [phase, setPhase] = useState<Phase>("idle");
  const [message, setMessage] = useState<string | null>(null);

  const signIn = async () => {
    setPhase("pending");
    setMessage(null);
    try {
      const outcome = await signInWithPasskey();
      if (outcome === "ok") {
        platform.navigateTo(target);
        return;
      }
      setMessage(
        outcome === "unsupported"
          ? t("auth.login.unsupported")
          : outcome === "cancelled"
            ? t("auth.login.cancelled")
            : t("auth.login.failed"),
      );
    } catch (error) {
      setMessage(
        error instanceof ApiError
          ? errorText(t, error.code, error.params)
          : t("auth.login.failed"),
      );
    }
    setPhase("idle");
  };

  if (status.error !== null) {
    return (
      <div className="mx-auto w-full max-w-md p-4">
        <PageError
          title="auth.login.error"
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
        <Skeleton className="h-32" />
      </div>
    );
  }
  if (!status.data.enabled) {
    return <Navigate to="/" replace />;
  }
  if (status.data.authenticated) {
    return <GoTo path={target} />;
  }
  if (status.data.setup_required) {
    return <Navigate to="/setup" search={nextSearch(next)} replace />;
  }
  return (
    <div className="mx-auto w-full max-w-md space-y-4 p-4">
      <Card className="space-y-4 p-4">
        <h1 className="text-2xl font-semibold">{t("auth.login.title")}</h1>
        <p className="text-sm text-muted-foreground">{t("auth.login.body")}</p>
        <Button
          disabled={phase === "pending"}
          onClick={() => {
            void signIn();
          }}
        >
          {phase === "pending"
            ? t("auth.login.pending")
            : t("auth.login.submit")}
        </Button>
        {message !== null && (
          <p role="alert" className="text-sm text-danger-foreground">
            {message}
          </p>
        )}
        <Link
          to="/setup"
          search={nextSearch(next)}
          className="inline-flex min-h-11 items-center text-sm font-medium text-primary underline underline-offset-4"
        >
          {t("auth.login.setupLink")}
        </Link>
      </Card>
    </div>
  );
}
