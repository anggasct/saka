import { useT } from "@/i18n/use-t";
import { PageError } from "@/features/delivery/page-error";
import { Card } from "@/ui/card";
import { Skeleton } from "@/ui/skeleton";

import { InboxCard } from "./inbox-card";
import { useInbox, useInboxSummary } from "./queries";

function Empty() {
  const t = useT();
  const summary = useInboxSummary();
  return (
    <Card className="space-y-3 p-4">
      <h2 className="font-semibold">{t("inbox.empty.title")}</h2>
      <p className="text-sm text-muted-foreground">{t("inbox.empty.body")}</p>
      {summary.data !== undefined && (
        <dl className="grid grid-cols-[max-content_1fr] gap-x-4 gap-y-1 text-sm">
          <dt className="font-medium">{t("inbox.empty.running")}</dt>
          <dd className="tabular-nums">{summary.data.running}</dd>
          <dt className="font-medium">{t("inbox.empty.merged")}</dt>
          <dd className="tabular-nums">{summary.data.merged_today}</dd>
        </dl>
      )}
    </Card>
  );
}

export function InboxPage() {
  const t = useT();
  const inbox = useInbox();
  const items = inbox.data ?? [];
  return (
    <div className="mx-auto w-full max-w-4xl space-y-4 p-4">
      <div className="flex flex-wrap items-baseline justify-between gap-2">
        <h1 className="text-2xl font-semibold">{t("inbox.title")}</h1>
        {items.length > 0 && (
          <p className="text-sm text-muted-foreground">
            {t("inbox.summary", { count: items.length })}
          </p>
        )}
      </div>
      {inbox.error !== null ? (
        <PageError
          title="inbox.error.title"
          error={inbox.error}
          onRetry={() => {
            void inbox.refetch();
          }}
        />
      ) : inbox.isPending ? (
        <div
          aria-busy="true"
          aria-label={t("inbox.loading")}
          role="status"
          className="space-y-3"
        >
          <Skeleton className="h-24" />
          <Skeleton className="h-24" />
        </div>
      ) : items.length === 0 ? (
        <Empty />
      ) : (
        <ul className="space-y-3">
          {items.map((item) => (
            <li key={item.id}>
              <InboxCard item={item} />
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
