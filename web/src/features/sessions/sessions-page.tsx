import { Link } from "@tanstack/react-router";

import { PageError } from "@/features/delivery/page-error";
import { label } from "@/features/delivery/text";
import { When } from "@/features/delivery/when";
import { useT } from "@/i18n/use-t";
import { Badge } from "@/ui/badge";
import { Button } from "@/ui/button";
import { Card } from "@/ui/card";
import { Skeleton } from "@/ui/skeleton";

import type { ActiveSession } from "./queries";
import { useActiveSessions } from "./queries";
import { sessionTone } from "./text";

function Row({ session }: { session: ActiveSession }) {
  const t = useT();
  return (
    <li>
      <Link
        to="/sessions/$sessionId"
        params={{ sessionId: session.id }}
        className="block min-h-11 space-y-2 rounded-md border border-border bg-card p-3 text-sm hover:bg-muted focus-visible:outline-2 focus-visible:outline-ring"
      >
        <span className="flex flex-wrap items-center justify-between gap-2">
          <span className="font-semibold break-words">{session.title}</span>
          <Badge tone={sessionTone(session.state)}>
            {label(t, "session.state", session.state)}
          </Badge>
        </span>
        <span className="flex flex-wrap items-center gap-x-3 gap-y-1 text-muted-foreground">
          <span>{session.project_name}</span>
          <span>{label(t, "session.role", session.role)}</span>
          {session.model != null && <span>{session.model}</span>}
          <span>
            {t("sessions.lastActivity")} <When iso={session.last_activity_at} />
          </span>
        </span>
        {(session.pending.permissions > 0 || session.pending.decisions > 0) && (
          <span className="flex flex-wrap gap-2">
            {session.pending.permissions > 0 && (
              <Badge tone="attention">
                {t("task.pendingPermissions", {
                  count: session.pending.permissions,
                })}
              </Badge>
            )}
            {session.pending.decisions > 0 && (
              <Badge tone="attention">
                {t("sessions.pendingDecisions", {
                  count: session.pending.decisions,
                })}
              </Badge>
            )}
          </span>
        )}
      </Link>
    </li>
  );
}

export function SessionsPage() {
  const t = useT();
  const sessions = useActiveSessions();
  const items = sessions.data?.pages.flatMap((page) => page.items) ?? [];
  return (
    <div className="mx-auto w-full max-w-4xl space-y-4 p-4">
      <div className="flex flex-wrap items-baseline justify-between gap-2">
        <h1 className="text-2xl font-semibold">{t("sessions.title")}</h1>
        {items.length > 0 && (
          <p className="text-sm text-muted-foreground">
            {t("sessions.summary", { count: items.length })}
          </p>
        )}
      </div>
      {sessions.isError ? (
        <PageError
          title="sessions.error.title"
          error={sessions.error}
          onRetry={() => {
            void sessions.refetch();
          }}
        />
      ) : sessions.isPending ? (
        <div
          aria-busy="true"
          aria-label={t("sessions.loading")}
          role="status"
          className="space-y-3"
        >
          <Skeleton className="h-20" />
          <Skeleton className="h-20" />
        </div>
      ) : items.length === 0 ? (
        <Card className="space-y-2 p-4">
          <h2 className="font-semibold">{t("sessions.empty.title")}</h2>
          <p className="text-sm text-muted-foreground">
            {t("sessions.empty.body")}
          </p>
        </Card>
      ) : (
        <>
          <ul className="space-y-3">
            {items.map((session) => (
              <Row key={session.id} session={session} />
            ))}
          </ul>
          {sessions.hasNextPage && (
            <Button
              variant="outline"
              disabled={sessions.isFetchingNextPage}
              onClick={() => {
                void sessions.fetchNextPage();
              }}
            >
              {t("sessions.more")}
            </Button>
          )}
        </>
      )}
    </div>
  );
}
