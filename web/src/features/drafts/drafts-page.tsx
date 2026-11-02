import { Link } from "@tanstack/react-router";
import { useMemo } from "react";

import { PageError } from "@/features/delivery/page-error";
import { When } from "@/features/delivery/when";
import { useDocTree } from "@/features/docs/queries";
import { useT } from "@/i18n/use-t";
import { Badge } from "@/ui/badge";
import { Card } from "@/ui/card";
import { cn } from "@/ui/cn";
import { Skeleton } from "@/ui/skeleton";

import { DraftDialog } from "./draft-dialog";
import { useProjectDrafts, type DraftFilter, type SpecDraft } from "./queries";
import { roundText, stateText, stateTone } from "./text";

const PAGE = "mx-auto w-full max-w-5xl space-y-4 p-4";
const FILTERS: readonly DraftFilter[] = ["active", "all"];

function NewDraft({
  projectId,
  primary,
}: {
  projectId: string;
  primary: boolean;
}) {
  const tree = useDocTree(projectId);
  const files = useMemo(
    () => (tree.data?.files ?? []).map((f) => f.path),
    [tree.data],
  );
  const existing = useMemo(() => new Set(files), [files]);
  if (tree.data === undefined) {
    return null;
  }
  return (
    <DraftDialog
      projectId={projectId}
      projectPath={tree.data.project_path}
      branch={tree.data.base_branch}
      files={files}
      existing={existing}
      triggerVariant={primary ? "default" : "outline"}
    />
  );
}

function attemptText(t: ReturnType<typeof useT>, draft: SpecDraft): string {
  return draft.attempt
    ? t("draft.attemptNumber", { number: draft.attempt.number })
    : t("draft.noAttempt");
}

function Facts({ draft }: { draft: SpecDraft }) {
  const t = useT();
  const round = roundText(t, draft);
  return (
    <p className="flex flex-wrap gap-x-3 text-xs break-normal text-muted-foreground md:hidden">
      <span className="sm:hidden">{attemptText(t, draft)}</span>
      {round !== null && <span className="sm:hidden">{round}</span>}
      <When iso={draft.updated_at} />
    </p>
  );
}

export function DraftsPage({
  projectId,
  filter,
}: {
  projectId: string;
  filter: DraftFilter;
}) {
  const t = useT();
  const drafts = useProjectDrafts(projectId, filter);
  return (
    <div className={PAGE}>
      <Link
        to="/docs/$projectId"
        params={{ projectId }}
        className="inline-flex min-h-11 items-center text-sm font-medium text-primary underline underline-offset-4"
      >
        {t("draft.back.docs")}
      </Link>
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h1 className="text-2xl font-semibold">{t("draft.list.title")}</h1>
        {drafts.data?.length !== 0 && (
          <NewDraft projectId={projectId} primary={false} />
        )}
      </div>
      <nav aria-label={t("draft.list.filter")} className="flex gap-2">
        {FILTERS.map((value) => (
          <Link
            key={value}
            to="/docs/$projectId/drafts"
            params={{ projectId }}
            search={value === "all" ? { state: "all" } : {}}
            aria-current={value === filter ? "page" : undefined}
            className={cn(
              "inline-flex min-h-11 items-center rounded-md border border-border px-4 text-sm font-medium",
              value === filter
                ? "bg-primary text-primary-foreground"
                : "bg-background hover:bg-muted",
            )}
          >
            {t(value === "all" ? "draft.list.all" : "draft.list.active")}
          </Link>
        ))}
      </nav>
      {drafts.isPending ? (
        <div
          role="status"
          aria-busy="true"
          aria-label={t("draft.list.loading")}
        >
          <Skeleton className="h-40" />
        </div>
      ) : drafts.isError ? (
        <PageError
          title="draft.list.error"
          error={drafts.error}
          onRetry={() => void drafts.refetch()}
        />
      ) : drafts.data.length === 0 ? (
        <Card className="space-y-3 p-4">
          <h2 className="font-semibold">
            {t(filter === "all" ? "draft.list.empty" : "draft.list.noActive")}
          </h2>
          <p className="text-sm text-muted-foreground">
            {t("draft.list.emptyBody")}
          </p>
          <NewDraft projectId={projectId} primary />
        </Card>
      ) : (
        <Card className="overflow-x-auto p-0">
          <table className="w-full text-left text-sm">
            <caption className="sr-only">{t("draft.list.title")}</caption>
            <thead className="border-b border-border text-xs text-muted-foreground">
              <tr>
                <th scope="col" className="p-3 font-medium">
                  {t("draft.list.path")}
                </th>
                <th scope="col" className="p-3 font-medium">
                  {t("draft.list.state")}
                </th>
                <th
                  scope="col"
                  className="hidden p-3 font-medium sm:table-cell"
                >
                  {t("draft.list.attempt")}
                </th>
                <th
                  scope="col"
                  className="hidden p-3 font-medium sm:table-cell"
                >
                  {t("draft.list.round")}
                </th>
                <th
                  scope="col"
                  className="hidden p-3 font-medium md:table-cell"
                >
                  {t("draft.list.time")}
                </th>
              </tr>
            </thead>
            <tbody className="divide-y divide-border">
              {drafts.data.map((draft) => (
                <tr key={draft.id}>
                  <td className="p-3 break-all">
                    <Link
                      to="/docs/$projectId/drafts/$draftId"
                      params={{ projectId, draftId: draft.id }}
                      className="inline-flex min-h-11 items-center font-mono text-primary underline underline-offset-4"
                    >
                      {draft.path}
                    </Link>
                    <Facts draft={draft} />
                  </td>
                  <td className="p-3">
                    <Badge tone={stateTone(draft.state)}>
                      {stateText(t, draft.state)}
                    </Badge>
                  </td>
                  <td className="hidden p-3 sm:table-cell">
                    {attemptText(t, draft)}
                  </td>
                  <td className="hidden p-3 sm:table-cell">
                    {roundText(t, draft) ?? t("draft.noAttempt")}
                  </td>
                  <td className="hidden p-3 md:table-cell">
                    <When iso={draft.updated_at} />
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </Card>
      )}
    </div>
  );
}
