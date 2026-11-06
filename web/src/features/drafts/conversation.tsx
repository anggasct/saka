import { useQueryClient } from "@tanstack/react-query";
import { useId, useRef, useState } from "react";

import { ApiError, newIdempotencyKey } from "@/api/client";
import { QUERY_ROOTS } from "@/api/events";
import { ActionDialog } from "@/features/delivery/action-dialog";
import { codeOf } from "@/features/delivery/page-error";
import { Section } from "@/features/delivery/section";
import { label, text } from "@/features/delivery/text";
import { useT } from "@/i18n/use-t";
import { Button } from "@/ui/button";
import { Card } from "@/ui/card";
import { Skeleton } from "@/ui/skeleton";

import {
  commitDraft,
  discardDraft,
  messageDraft,
  useDraftChanges,
  type SpecDraft,
} from "./queries";

export const MESSAGE_MAX = 4000;
export const COMMIT_MESSAGE_MAX = 200;
const RELOAD_ON = [
  "invalid_state",
  "draft_not_waiting",
  "draft_decision_open",
  "draft_lint_failed",
];

export function useAfterOperatorAction() {
  const queryClient = useQueryClient();
  const refresh = () =>
    Promise.all([
      queryClient.invalidateQueries({ queryKey: [QUERY_ROOTS.specDrafts] }),
      queryClient.invalidateQueries({ queryKey: [QUERY_ROOTS.sessions] }),
    ]);
  return async (action: () => Promise<void>) => {
    try {
      await action();
    } catch (e) {
      if (RELOAD_ON.includes(codeOf(e))) {
        await refresh();
      }
      throw e;
    }
    await refresh();
  };
}

export function useOperatorExplain() {
  const t = useT();
  return (error: unknown): string | null => {
    if (!(error instanceof ApiError)) {
      return null;
    }
    switch (error.code) {
      case "draft_not_waiting":
      case "draft_lint_failed":
      case "draft_decision_open":
      case "draft_no_changes":
        return t(`draft.error.${error.code}`);
      case "draft_base_moved":
        return t("draft.error.draft_base_moved", {
          paths: text(error.params.paths),
        });
      default:
        return null;
    }
  };
}

export function MessageForm({ draft }: { draft: SpecDraft }) {
  const t = useT();
  const after = useAfterOperatorAction();
  const explain = useOperatorExplain();
  const [value, setValue] = useState("");
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const retained = useRef<{ key: string; text: string } | null>(null);
  const fieldId = useId();
  const countId = useId();
  const trimmed = value.trim();
  const tooLong = value.length > MESSAGE_MAX;
  const blocked = pending || trimmed === "" || tooLong;

  const send = async () => {
    const kept = retained.current;
    const key =
      kept !== null && kept.text === trimmed ? kept.key : newIdempotencyKey();
    retained.current = null;
    setPending(true);
    setError(null);
    try {
      await after(() => messageDraft(draft.id, trimmed, key));
      setValue("");
    } catch (e) {
      setError(e);
      if (!(e instanceof ApiError)) {
        retained.current = { key, text: trimmed };
      }
    } finally {
      setPending(false);
    }
  };

  return (
    <form
      className="space-y-2"
      onSubmit={(event) => {
        event.preventDefault();
        if (!blocked) {
          void send();
        }
      }}
    >
      <label htmlFor={fieldId} className="text-sm font-medium">
        {t("draft.message.label")}
      </label>
      <textarea
        id={fieldId}
        value={value}
        rows={4}
        aria-describedby={countId}
        aria-invalid={tooLong}
        onChange={(event) => {
          setValue(event.target.value);
        }}
        className="w-full rounded-md border border-border bg-background p-2 text-sm"
      />
      <p id={countId} className="text-xs text-muted-foreground">
        {t("dialog.count", { count: value.length, max: MESSAGE_MAX })}
      </p>
      {error !== null && (
        <p role="alert" className="text-sm text-danger-foreground">
          {explain(error) ?? t("draft.message.failed")}
        </p>
      )}
      <Button type="submit" disabled={blocked}>
        {t("draft.message.send")}
      </Button>
    </form>
  );
}

export function CommitDraftDialog({ draft }: { draft: SpecDraft }) {
  const t = useT();
  const after = useAfterOperatorAction();
  const explain = useOperatorExplain();
  return (
    <ActionDialog
      trigger={t("draft.commit.trigger")}
      triggerVariant="default"
      title={t("draft.commit.title")}
      description={t("draft.commit.description")}
      note={{ label: t("draft.commit.message"), max: COMMIT_MESSAGE_MAX }}
      submit={t("draft.commit.submit")}
      explain={explain}
      onSubmit={(_, key, note) => after(() => commitDraft(draft.id, note, key))}
    />
  );
}

export function DiscardDraftDialog({ draft }: { draft: SpecDraft }) {
  const t = useT();
  const after = useAfterOperatorAction();
  const explain = useOperatorExplain();
  return (
    <ActionDialog
      trigger={t("draft.discard.trigger")}
      title={t("draft.discard.title")}
      description={t("draft.discard.description")}
      reason={{ label: t("dialog.reason"), required: true }}
      submit={t("draft.discard.submit")}
      explain={explain}
      onSubmit={(reason, key) =>
        after(() => discardDraft(draft.id, reason ?? "", key))
      }
    />
  );
}

export function WaitingCard({ draft }: { draft: SpecDraft }) {
  const t = useT();
  const actions = draft.available_actions;
  return (
    <Card className="space-y-3 p-4">
      <h2 className="font-semibold">{t("draft.waiting.title")}</h2>
      <p className="text-sm">{t("draft.stateBody.awaiting_operator")}</p>
      {!actions.includes("commit") && (
        <p className="text-sm text-muted-foreground">
          {t("draft.commit.blocked")}
        </p>
      )}
      <div className="flex flex-wrap gap-2">
        {actions.includes("commit") && <CommitDraftDialog draft={draft} />}
        {actions.includes("discard") && <DiscardDraftDialog draft={draft} />}
      </div>
    </Card>
  );
}

export function ConversationSection({ draft }: { draft: SpecDraft }) {
  const t = useT();
  const changes = useDraftChanges(draft.id, draft.state !== "queued");
  const canMessage = draft.available_actions.includes("message");
  return (
    <>
      <Section title={t("draft.changes.title")}>
        {changes.isPending ? (
          <Skeleton className="h-12" />
        ) : changes.isError ? (
          <p role="alert" className="text-sm text-danger-foreground">
            {t("draft.changes.failed")}
          </p>
        ) : (
          <>
            <p className="font-medium">
              {changes.data.lint.passed
                ? t("draft.changes.lintPassed")
                : t("draft.changes.lintFailed", {
                    count: changes.data.lint.findings.length,
                  })}
            </p>
            {changes.data.files.length === 0 ? (
              <p className="text-sm text-muted-foreground">
                {t("draft.changes.none")}
              </p>
            ) : (
              <ul className="space-y-2">
                {changes.data.files.map((file) => (
                  <li key={file.path}>
                    <details className="rounded border border-border">
                      <summary className="flex min-h-11 cursor-pointer flex-wrap items-center gap-x-2 px-2 text-sm">
                        <span className="font-medium">
                          {label(t, "draft.changes", file.status)}
                        </span>
                        <code className="break-all">{file.path}</code>
                      </summary>
                      <pre
                        aria-label={t("draft.changes.diff", {
                          path: file.path,
                        })}
                        className="bg-muted p-2 text-xs break-words whitespace-pre-wrap"
                      >
                        {file.diff}
                      </pre>
                      {file.truncated && (
                        <p className="p-2 text-xs text-muted-foreground">
                          {t("draft.changes.truncated")}
                        </p>
                      )}
                    </details>
                  </li>
                ))}
              </ul>
            )}
          </>
        )}
      </Section>
      {canMessage && (
        <Section title={t("draft.message.title")}>
          <MessageForm draft={draft} />
        </Section>
      )}
    </>
  );
}
