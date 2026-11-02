import { Link } from "@tanstack/react-router";

import { useT } from "@/i18n/use-t";

import { useProjectDrafts } from "./queries";

export function DraftsLink({ projectId }: { projectId: string }) {
  const t = useT();
  const active = useProjectDrafts(projectId, "active");
  const count = active.data?.length;
  return (
    <Link
      to="/docs/$projectId/drafts"
      params={{ projectId }}
      className="inline-flex min-h-11 items-center rounded-md px-2 text-sm font-medium text-primary underline underline-offset-4"
    >
      {count === undefined
        ? t("draft.list.link")
        : t("draft.list.linkCount", { count })}
    </Link>
  );
}
