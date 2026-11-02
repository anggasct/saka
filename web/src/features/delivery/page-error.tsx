import { ApiError } from "@/api/client";
import { useT } from "@/i18n/use-t";
import type { MessageKey } from "@/i18n/messages";
import { Button } from "@/ui/button";
import { Card } from "@/ui/card";

import { errorText } from "./text";

export function codeOf(error: unknown): string {
  return error instanceof ApiError ? error.code : "network_error";
}

export function PageError({
  title,
  error,
  onRetry,
}: {
  title: MessageKey;
  error: unknown;
  onRetry: () => void;
}) {
  const t = useT();
  return (
    <Card role="alert" className="space-y-3 p-4">
      <h2 className="font-semibold">{t(title)}</h2>
      <p className="text-sm text-muted-foreground">
        {errorText(t, codeOf(error))}
      </p>
      <Button onClick={onRetry}>{t("delivery.error.retry")}</Button>
    </Card>
  );
}
