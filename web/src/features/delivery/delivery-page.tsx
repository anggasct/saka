import { useT } from "@/i18n/use-t";
import { Card } from "@/ui/card";
import { Skeleton } from "@/ui/skeleton";

import { PageError } from "./page-error";
import { PermissionPanel } from "./permission-panel";
import { ProjectSection } from "./project-section";
import { useProjects, useSpecsOf } from "./queries";

const ADD_PROJECT = "saka project add <repo-url> --path docs";
const APPROVE_SPEC = "saka spec approve <project> docs/specs/<spec>.md";

export function DeliveryPage() {
  const t = useT();
  const projects = useProjects();
  const specs = useSpecsOf((projects.data ?? []).map((p) => p.id));
  const loading = projects.isPending || specs.some((s) => s.isPending);
  const failed = projects.error ?? specs.find((s) => s.error)?.error ?? null;
  const anySpec = specs.some((s) => (s.data ?? []).length > 0);
  const takesOtherEntries = (projects.data ?? []).some(
    (p) => p.workflow.change_request != null || p.workflow.issue != null,
  );

  const retry = () => {
    void projects.refetch();
    for (const s of specs) {
      void s.refetch();
    }
  };

  return (
    <div className="mx-auto w-full max-w-4xl space-y-4 p-4">
      <h1 className="text-2xl font-semibold">{t("delivery.title")}</h1>
      <PermissionPanel projects={projects.data ?? []} />
      {failed !== null ? (
        <PageError
          title="delivery.error.title"
          error={failed}
          onRetry={retry}
        />
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
      ) : !anySpec && !takesOtherEntries ? (
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
