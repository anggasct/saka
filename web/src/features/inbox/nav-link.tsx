import { Link } from "@tanstack/react-router";

import { useT } from "@/i18n/use-t";
import { Badge } from "@/ui/badge";

import { useInboxSummary } from "./queries";

export function InboxNavLink({ className }: { className: string }) {
  const t = useT();
  const summary = useInboxSummary();
  const open = summary.data?.open ?? 0;
  return (
    <Link to="/" className={className}>
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
