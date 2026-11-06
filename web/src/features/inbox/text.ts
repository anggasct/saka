import type { Relative, T } from "@/features/delivery/text";
import { blockerText, record, text } from "@/features/delivery/text";
import type { BadgeTone } from "@/ui/badge";

import type { InboxItem } from "./queries";

export function kindTone(kind: string): BadgeTone {
  return kind === "blocker" || kind === "spec_unclear" ? "danger" : "attention";
}

export function heading(t: T, item: InboxItem): string {
  const { task_key, spec_title, spec_slug, draft_path } = item.title;
  if (task_key != null) {
    return task_key;
  }
  if (spec_title != null || spec_slug != null) {
    return spec_title ?? spec_slug ?? "";
  }
  return draft_path == null ? "" : t("inbox.draft", { path: draft_path });
}

export function sentence(t: T, item: InboxItem, relative: Relative): string {
  const params = record(item.params);
  switch (item.subject) {
    case "blocker":
      return blockerText(t, text(params.code), params, relative);
    case "permission":
      return t("permission.title", { tool: text(params.tool) });
    case "spec":
      return t("inbox.item.spec");
    case "step_start":
      return t("inbox.item.step_start");
    case "merge":
      return t("inbox.item.merge");
    case "decision":
      return text(params.question) || t("inbox.item.decision");
    default:
      return t("inbox.item.decision");
  }
}
