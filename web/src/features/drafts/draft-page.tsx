import { useQueryClient } from "@tanstack/react-query";
import { Link, useNavigate } from "@tanstack/react-router";
import { useEffect, useRef } from "react";

import { ApiError } from "@/api/client";
import { QUERY_ROOTS } from "@/api/events";
import { ActionDialog } from "@/features/delivery/action-dialog";
import { BudgetList } from "@/features/delivery/budget-list";
import { useFormat } from "@/features/delivery/format";
import { codeOf, PageError } from "@/features/delivery/page-error";
import { useProject } from "@/features/delivery/queries";
import { Section } from "@/features/delivery/section";
import { DecisionSection } from "@/features/decisions/decision-section";
import { SessionSection } from "@/features/delivery/session-section";
import { label, record, sha } from "@/features/delivery/text";
import { When } from "@/features/delivery/when";
import { useT } from "@/i18n/use-t";
import { Badge } from "@/ui/badge";
import { Card } from "@/ui/card";
import { Skeleton } from "@/ui/skeleton";
import { lintMessage } from "@/wasm/diagnostics";
import type { LintIssue } from "@/wasm/spec-tools";

import { cancelDraft, retryDraft, useDraft, type SpecDraft } from "./queries";
import {
  failureText,
  roundText,
  stateText,
  stateTone,
  waitingText,
} from "./text";

const PAGE = "mx-auto w-full max-w-4xl space-y-4 p-4";
const NOTE_MAX = 4000;
const RELOAD_ON = ["invalid_state", "retry_limit_reached"];
const OPEN_DECISION = "decision_unanswered";

function useAfterAction() {
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

function useExplain() {
  const t = useT();
  return (error: unknown): string | null => {
    if (!(error instanceof ApiError)) {
      return null;
    }
    switch (error.code) {
      case "retry_limit_reached":
        return t("draft.error.retry_limit_reached", {
          limit: Number(error.params.limit) || 0,
        });
      case "invalid_state":
        return t("draft.error.invalid_state");
      default:
        return null;
    }
  };
}

function RetryDraftDialog({
  draft,
  primary,
}: {
  draft: SpecDraft;
  primary: boolean;
}) {
  const t = useT();
  const after = useAfterAction();
  const explain = useExplain();
  return (
    <ActionDialog
      trigger={t("draft.retry.trigger")}
      triggerVariant={primary ? "default" : "outline"}
      title={t("draft.retry.title")}
      description={t("draft.retry.description")}
      reason={{ label: t("dialog.reason"), required: true }}
      note={{ label: t("draft.retry.note"), max: NOTE_MAX }}
      submit={t("draft.retry.submit")}
      explain={explain}
      onSubmit={(reason, key, note) =>
        after(() => retryDraft(draft.id, reason ?? "", note, key))
      }
    />
  );
}

function CancelDraftDialog({ draft }: { draft: SpecDraft }) {
  const t = useT();
  const after = useAfterAction();
  const explain = useExplain();
  return (
    <ActionDialog
      trigger={t("draft.cancel.trigger")}
      title={t("draft.cancel.title")}
      description={t("draft.cancel.description")}
      reason={{ label: t("dialog.reason"), required: true }}
      submit={t("draft.cancel.submit")}
      explain={explain}
      onSubmit={(reason, key) =>
        after(() => cancelDraft(draft.id, reason ?? "", key))
      }
    />
  );
}

const LINK_BUTTON =
  "inline-flex min-h-11 items-center justify-center rounded-md bg-primary px-4 text-sm font-medium text-primary-foreground hover:bg-primary/90 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-ring";

function PrimaryAction({
  draft,
  projectId,
}: {
  draft: SpecDraft;
  projectId: string;
}) {
  const t = useT();
  switch (draft.primary_action) {
    case "retry":
      return <RetryDraftDialog draft={draft} primary />;
    case "open_written":
      return (
        <Link
          to="/docs/$projectId"
          params={{ projectId }}
          search={{ path: draft.path, draft: draft.id }}
          className={LINK_BUTTON}
        >
          {t("draft.openWritten")}
        </Link>
      );
    case "open_file":
      return (
        <Link
          to="/docs/$projectId"
          params={{ projectId }}
          search={{ path: draft.path }}
          className={LINK_BUTTON}
        >
          {t("draft.openFile")}
        </Link>
      );
    case null:
    case undefined:
    default:
      return null;
  }
}

function FailureCard({
  draft,
  projectId,
}: {
  draft: SpecDraft;
  projectId: string;
}) {
  const t = useT();
  const format = useFormat();
  const failure = draft.failure;
  const canRetry = draft.available_actions.includes("retry");
  const canCancel = draft.available_actions.includes("cancel");
  return (
    <Card role="alert" className="space-y-3 border-danger p-4">
      <h2 className="font-semibold">{t("draft.failed.title")}</h2>
      {failure && (
        <p className="text-sm break-words">
          {failureText(
            t,
            failure.code,
            record(failure.params),
            format.relative,
          )}
        </p>
      )}
      {draft.retries > 0 && (
        <p className="text-sm text-muted-foreground">
          {t("draft.retries", { count: draft.retries })}
        </p>
      )}
      <div className="flex flex-wrap gap-2">
        <PrimaryAction draft={draft} projectId={projectId} />
        {canRetry && draft.primary_action !== "retry" && (
          <RetryDraftDialog draft={draft} primary={false} />
        )}
        {canCancel && <CancelDraftDialog draft={draft} />}
      </div>
    </Card>
  );
}

function StateCard({
  draft,
  projectId,
}: {
  draft: SpecDraft;
  projectId: string;
}) {
  const t = useT();
  const format = useFormat();
  switch (draft.state) {
    case "failed":
      return <FailureCard draft={draft} projectId={projectId} />;
    case "queued":
      return (
        <Card className="space-y-2 p-4">
          <h2 className="font-semibold">{t("draft.queued.title")}</h2>
          {draft.waiting_on.length === 0 ? (
            <p className="text-sm">{t("draft.queued.body")}</p>
          ) : (
            <ul className="space-y-1 text-sm">
              {draft.waiting_on.map((item) => (
                <li key={item.kind}>
                  {waitingText(t, item, format.relative, format.percent)}
                </li>
              ))}
            </ul>
          )}
        </Card>
      );
    case "succeeded":
      return (
        <Card className="space-y-3 p-4">
          <h2 className="font-semibold">{t("draft.succeeded.title")}</h2>
          <p className="text-sm">
            {t("draft.succeeded.body", { commit: sha(draft.commit) })}
          </p>
          <Link
            to="/docs/$projectId"
            params={{ projectId }}
            search={{ path: draft.path }}
            className={LINK_BUTTON}
          >
            {t("draft.openEditor")}
          </Link>
        </Card>
      );
    default:
      return (
        <Card className="p-4">
          <p className="text-sm">{label(t, "draft.stateBody", draft.state)}</p>
        </Card>
      );
  }
}

function LintReport({ draft }: { draft: SpecDraft }) {
  const t = useT();
  const round = draft.last_round;
  if (!round) {
    return null;
  }
  const issues: LintIssue[] = round.issues.map((issue) => ({
    code: issue.code,
    line: issue.line,
    severity: issue.severity === "warning" ? "warning" : "error",
    params: record(issue.params),
  }));
  return (
    <Section title={t("draft.lint.title")}>
      <p className="text-sm text-muted-foreground">
        {t("draft.lint.round", { attempt: round.attempt, round: round.round })}
      </p>
      <p className="font-medium">
        {t("draft.lint.summary", {
          outcome: round.outcome,
          issues: issues.filter((i) => i.code !== OPEN_DECISION).length,
          open: round.decisions_open,
        })}
      </p>
      {issues.length > 0 && (
        <ul className="divide-y divide-border text-sm">
          {issues.map((issue, index) => (
            <li
              key={`${issue.code}-${String(issue.line)}-${String(index)}`}
              className="flex flex-wrap gap-x-3 py-2"
            >
              <span className="font-mono text-muted-foreground">
                {t("draft.lint.line", { line: issue.line })}
              </span>
              <span className="min-w-0 flex-1 break-words">
                {lintMessage(issue, t)}
              </span>
            </li>
          ))}
        </ul>
      )}
    </Section>
  );
}

function Header({ draft, projectId }: { draft: SpecDraft; projectId: string }) {
  const t = useT();
  const round = roundText(t, draft);
  const attempt = draft.attempt;
  return (
    <header className="space-y-3">
      <Link
        to="/docs/$projectId/drafts"
        params={{ projectId }}
        className="inline-flex min-h-11 items-center text-sm font-medium text-primary underline underline-offset-4"
      >
        {t("draft.back.list")}
      </Link>
      <div className="flex flex-wrap items-center gap-2">
        <h1 className="min-w-0 font-mono text-xl font-semibold break-all">
          {draft.path}
        </h1>
        <Badge tone={stateTone(draft.state)}>{stateText(t, draft.state)}</Badge>
      </div>
      <dl className="grid grid-cols-1 gap-x-4 gap-y-1 text-sm sm:grid-cols-[max-content_1fr]">
        <dt className="font-medium">{t("draft.list.attempt")}</dt>
        <dd>
          {attempt
            ? t("draft.attempt", {
                number: attempt.number,
                trigger: attempt.trigger,
              })
            : t("draft.noAttempt")}
        </dd>
        {round !== null && (
          <>
            <dt className="font-medium">{t("draft.list.round")}</dt>
            <dd>{round}</dd>
          </>
        )}
        <dt className="font-medium">{t("draft.new.language")}</dt>
        <dd>{label(t, "language", draft.locale)}</dd>
        <dt className="font-medium">{t("draft.new.context")}</dt>
        <dd className="break-all">
          {draft.context_paths.length === 0 ? (
            t("draft.contextNone")
          ) : (
            <ul>
              {draft.context_paths.map((p) => (
                <li key={p}>
                  <code>{p}</code>
                </li>
              ))}
            </ul>
          )}
        </dd>
        <dt className="font-medium">{t("draft.created")}</dt>
        <dd>
          <When iso={draft.created_at} />
        </dd>
      </dl>
      {draft.idea && (
        <details>
          <summary className="inline-flex min-h-11 cursor-pointer items-center text-sm font-medium">
            {t("draft.new.idea")}
          </summary>
          <p className="rounded bg-muted p-2 text-sm break-words whitespace-pre-wrap">
            {draft.idea}
          </p>
        </details>
      )}
      {draft.state !== "failed" &&
        draft.available_actions.includes("cancel") && (
          <div className="flex flex-wrap gap-2">
            <CancelDraftDialog draft={draft} />
          </div>
        )}
    </header>
  );
}

function useOpenWhenSucceeded(draft: SpecDraft | undefined, projectId: string) {
  const navigate = useNavigate();
  const seen = useRef<string | null>(null);
  const state = draft?.state;
  const path = draft?.path;
  useEffect(() => {
    if (state === undefined || path === undefined) {
      return;
    }
    const before = seen.current;
    seen.current = state;
    if (before !== null && before !== "succeeded" && state === "succeeded") {
      void navigate({
        to: "/docs/$projectId",
        params: { projectId },
        search: { path },
        replace: true,
      });
    }
  }, [state, path, projectId, navigate]);
}

export function DraftPage({
  projectId,
  draftId,
}: {
  projectId: string;
  draftId: string;
}) {
  const t = useT();
  const draft = useDraft(draftId);
  const project = useProject(projectId);
  useOpenWhenSucceeded(draft.data, projectId);

  if (draft.isPending) {
    return (
      <div
        aria-busy="true"
        aria-label={t("draft.loading")}
        role="status"
        className={PAGE}
      >
        <Skeleton className="h-24" />
        <Skeleton className="h-48" />
      </div>
    );
  }
  if (draft.isError) {
    if (draft.error instanceof ApiError && draft.error.status === 404) {
      return (
        <div className={PAGE}>
          <Card className="space-y-3 p-4">
            <h1 className="text-xl font-semibold">{t("draft.notFound")}</h1>
            <Link
              to="/docs/$projectId/drafts"
              params={{ projectId }}
              className="inline-flex min-h-11 items-center text-sm font-medium text-primary underline underline-offset-4"
            >
              {t("draft.back.list")}
            </Link>
          </Card>
        </div>
      );
    }
    return (
      <div className={PAGE}>
        <PageError
          title="draft.error.title"
          error={draft.error}
          onRetry={() => void draft.refetch()}
        />
      </div>
    );
  }
  const item = draft.data;
  return (
    <div className={PAGE}>
      <Header draft={item} projectId={projectId} />
      <StateCard draft={item} projectId={projectId} />
      {item.budget && (
        <Card className="p-4">
          <BudgetList title={t("draft.budget.title")} budget={item.budget} />
        </Card>
      )}
      <LintReport draft={item} />
      <DecisionSection run={{ kind: "spec_draft", id: item.id }} />
      {item.attempt && (
        <SessionSection
          owner={{ kind: "spec_draft", id: item.id }}
          attemptId={item.attempt.id}
          title={t("draft.session.title")}
          empty={t("draft.session.none")}
          project={project.data?.name}
          permissions={[]}
        />
      )}
    </div>
  );
}
