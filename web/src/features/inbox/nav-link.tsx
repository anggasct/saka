import { Link } from "@tanstack/react-router";

import { useT } from "@/i18n/use-t";
import { Badge } from "@/ui/badge";

import { useInboxSummary } from "./queries";

export function InboxNavLink() {
  const t = useT();
  const summary = useInboxSummary();
  const open = summary.data?.open ?? 0;
  return (
    <Link
      to="/"
      className="inline-flex min-h-11 items-center gap-2 text-sm [&.active]:font-semibold [&.active]:underline"
    >
      {t("nav.inbox")}
      {open > 0 && (
        <Badge tone="attention">
          <span aria-hidden="true">{open}</span>
          <span className="sr-only">
            {t("inbox.nav.waiting", { count: open })}
          </span>
        </Badge>
      )}
    </Link>
  );
}
