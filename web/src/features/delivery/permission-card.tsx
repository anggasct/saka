import { useQueryClient } from "@tanstack/react-query";
import { useId, useState } from "react";
import type { ReactNode } from "react";

import { ApiError, newIdempotencyKey } from "@/api/client";
import { QUERY_ROOTS } from "@/api/events";
import { useT } from "@/i18n/use-t";
import { Button } from "@/ui/button";

import { ActionDialog, fieldParams } from "./action-dialog";
import { codeOf } from "./page-error";
import type { Decision, PermissionRequest } from "./queries";
import { answerPermission } from "./queries";
import { errorText } from "./text";
import { When } from "./when";

type Answer = { decision: Decision; message: string | null; remember: boolean };

const ALLOW_ONCE: Answer = {
  decision: "allow",
  message: null,
  remember: false,
};

export function permissionAnchor(id: string): string {
  return `permission-${id}`;
}

function containsSecret(error: unknown): boolean {
  if (!(error instanceof ApiError) || error.code !== "validation_failed") {
    return false;
  }
  const field = fieldParams(error.params);
  return field.field === "remember" && field.code === "contains_secret";
}

export function PermissionCard({
  request,
  project,
  link,
}: {
  request: PermissionRequest;
  project: string | undefined;
  link?: ReactNode;
}) {
  const t = useT();
  const queryClient = useQueryClient();
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const titleId = useId();
  const where = project ?? t("permission.thisProject");

  const answer = async (body: Answer, key: string) => {
    try {
      await answerPermission(request.id, body, key);
    } catch (e) {
      if (codeOf(e) === "permission_already_answered") {
        setError(e);
        await queryClient.invalidateQueries({
          queryKey: [QUERY_ROOTS.permissions],
        });
      }
      throw e;
    }
    await Promise.all([
      queryClient.invalidateQueries({ queryKey: [QUERY_ROOTS.permissions] }),
      queryClient.invalidateQueries({ queryKey: [QUERY_ROOTS.workItems] }),
    ]);
  };

  const allow = async () => {
    setPending(true);
    setError(null);
    try {
      await answer(ALLOW_ONCE, newIdempotencyKey());
    } catch (e) {
      setError(e);
    } finally {
      setPending(false);
    }
  };

  return (
    <article
      id={permissionAnchor(request.id)}
      aria-labelledby={titleId}
      className="scroll-mt-4 space-y-2 rounded-md border border-attention bg-card p-3 text-sm"
    >
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h3 id={titleId} className="font-semibold break-words">
          {t("permission.title", { tool: request.tool })}
        </h3>
        <span className="text-xs text-muted-foreground">
          <When iso={request.requested_at} />
        </span>
      </div>
      {(project !== undefined || link !== undefined) && (
        <p className="flex flex-wrap items-center gap-x-3 text-muted-foreground">
          {project !== undefined && (
            <span className="break-words">{project}</span>
          )}
          {link}
        </p>
      )}
      <pre className="rounded bg-muted p-2 text-xs break-words whitespace-pre-wrap">
        {request.summary}
      </pre>
      <details>
        <summary className="inline-flex min-h-11 cursor-pointer items-center font-medium">
          {t("permission.input")}
        </summary>
        <pre className="rounded bg-muted p-2 text-xs break-words whitespace-pre-wrap">
          {JSON.stringify(request.input, null, 2)}
        </pre>
      </details>
      <div className="flex flex-wrap gap-2">
        <Button
          disabled={pending}
          onClick={() => {
            void allow();
          }}
        >
          {t("permission.allow")}
        </Button>
        <ActionDialog
          trigger={t("permission.deny.trigger")}
          title={t("permission.deny.title")}
          description={t("permission.deny.description", { tool: request.tool })}
          reason={{ label: t("permission.deny.message"), required: false }}
          submit={t("permission.deny.submit")}
          onSubmit={(message, key) =>
            answer({ decision: "deny", message, remember: false }, key)
          }
        />
        <ActionDialog
          trigger={t("permission.always.trigger")}
          title={t("permission.always.title")}
          description={
            request.tool === "Bash"
              ? t("permission.always.command", {
                  command: request.summary,
                  project: where,
                })
              : t("permission.always.tool", {
                  tool: request.tool,
                  project: where,
                })
          }
          submit={t("permission.always.submit")}
          explain={(e) => (containsSecret(e) ? t("permission.secret") : null)}
          alternative={(e) =>
            containsSecret(e)
              ? {
                  label: t("permission.once"),
                  run: (key) => answer(ALLOW_ONCE, key),
                }
              : null
          }
          onSubmit={(_, key) =>
            answer({ decision: "allow", message: null, remember: true }, key)
          }
        />
      </div>
      {error !== null && (
        <p role="alert" className="text-danger-foreground">
          {errorText(t, codeOf(error))}
        </p>
      )}
    </article>
  );
}
