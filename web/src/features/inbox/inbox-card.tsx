import { useId } from "react";

import { useFormat } from "@/features/delivery/format";
import { label, record, text } from "@/features/delivery/text";
import { When } from "@/features/delivery/when";
import { useT } from "@/i18n/use-t";
import { Badge } from "@/ui/badge";

import { Primary, Secondary } from "./inbox-action";
import type { InboxItem } from "./queries";
import { heading, kindTone, sentence } from "./text";

export function InboxCard({ item }: { item: InboxItem }) {
  const t = useT();
  const format = useFormat();
  const titleId = useId();
  const title = heading(t, item);
  const summary = text(record(item.params).summary);
  return (
    <article
      aria-labelledby={titleId}
      className="space-y-2 rounded-md border border-attention bg-card p-3 text-sm"
    >
      <div className="flex flex-wrap items-center justify-between gap-2">
        <div className="flex flex-wrap items-center gap-2">
          <Badge tone={kindTone(item.kind)}>
            {label(t, "inbox.kind", item.kind)}
          </Badge>
          <span className="break-words text-muted-foreground">
            {item.project_name}
          </span>
        </div>
        <span className="text-xs text-muted-foreground">
          <When iso={item.opened_at} />
        </span>
      </div>
      <h2 id={titleId} className="font-semibold break-words">
        {title}
      </h2>
      <p className="break-words">{sentence(t, item, format.relative)}</p>
      {item.subject === "permission" && summary !== "" && (
        <pre className="rounded bg-muted p-2 text-xs break-words whitespace-pre-wrap">
          {summary}
        </pre>
      )}
      <div className="flex flex-wrap gap-2">
        <Primary item={item} />
        <Secondary item={item} />
      </div>
    </article>
  );
}
