import { useT } from "@/i18n/use-t";
import { ApiError } from "@/api/client";
import { Badge } from "@/ui/badge";
import { Button } from "@/ui/button";
import { Card } from "@/ui/card";
import { Skeleton } from "@/ui/skeleton";

import { ProjectSection } from "./project-section";
import { usePendingPermissionCount, useProjects, useSpecsOf } from "./queries";
import { errorText } from "./text";

const ADD_PROJECT = "saka project add <repo-url> --path docs";
const APPROVE_SPEC = "saka spec approve <project> docs/specs/<spec>.md";

function codeOf(error: unknown): string {
  return error instanceof ApiError ? error.code : "network_error";
}

export function DeliveryPage() {
  const t = useT();
  const projects = useProjects();
  const specs = useSpecsOf((projects.data ?? []).map((p) => p.id));
  const pending = usePendingPermissionCount();
  const loading = projects.isPending || specs.some((s) => s.isPending);
  const failed = projects.error ?? specs.find((s) => s.error)?.error ?? null;
  const anySpec = specs.some((s) => (s.data ?? []).length > 0);

  const retry = () => {
    void projects.refetch();
    for (const s of specs) {
      void s.refetch();
    }
  };

  return (
    <div className="mx-auto w-full max-w-4xl space-y-4 p-4">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h1 className="text-2xl font-semibold">{t("delivery.title")}</h1>
        {pending.data !== undefined && pending.data > 0 && (
          <Badge tone="attention">
            {t("delivery.pendingPermissions", { count: pending.data })}
          </Badge>
        )}
      </div>
      {failed !== null ? (
        <Card role="alert" className="space-y-3 p-4">
          <h2 className="font-semibold">{t("delivery.error.title")}</h2>
          <p className="text-sm text-muted-foreground">
            {errorText(t, codeOf(failed))}
          </p>
          <Button onClick={retry}>{t("delivery.error.retry")}</Button>
        </Card>
      ) : loading ? (
        <div
          aria-busy="true"
          aria-label={t("delivery.loading")}
          role="status"
          className="space-y-3"
        >
          <Skeleton className="h-24" />
          <Skeleton className="h-24" />
        </div>
      ) : !anySpec ? (
        <Card className="space-y-3 p-4">
          <h2 className="font-semibold">{t("delivery.empty.title")}</h2>
          <p className="text-sm text-muted-foreground">
            {t("delivery.empty.body")}
          </p>
          <dl className="space-y-2 text-sm">
            <dt className="font-medium">{t("delivery.empty.addProject")}</dt>
            <dd>
              <code className="block overflow-x-auto rounded bg-muted p-2">
                {ADD_PROJECT}
              </code>
            </dd>
            <dt className="font-medium">{t("delivery.empty.approveSpec")}</dt>
            <dd>
              <code className="block overflow-x-auto rounded bg-muted p-2">
                {APPROVE_SPEC}
              </code>
            </dd>
          </dl>
        </Card>
      ) : (
        (projects.data ?? []).map((project, i) => (
          <ProjectSection
            key={project.id}
            project={project}
            specs={specs[i]?.data ?? []}
          />
        ))
      )}
    </div>
  );
}
