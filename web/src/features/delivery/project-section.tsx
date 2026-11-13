import { Link } from "@tanstack/react-router";

import { IssueGroup } from "@/features/issues/issue-group";
import { QuickDialog } from "@/features/quick/quick-dialog";
import { useT } from "@/i18n/use-t";
import { Card } from "@/ui/card";
import { Skeleton } from "@/ui/skeleton";

import type { ProjectItem, SpecItem } from "./queries";
import { useWorkItems } from "./queries";
import { ExternalGroup } from "./external-group";
import { RevertGroup } from "./revert-group";
import { SpecGroup } from "./spec-group";

export function ProjectSection({
  project,
  specs,
}: {
  project: ProjectItem;
  specs: readonly SpecItem[];
}) {
  const t = useT();
  const workItems = useWorkItems(project.id);
  const headingId = `project-${project.id}`;
  const outside = (workItems.data ?? []).filter(
    (w) => w.origin === "external_change",
  );
  const reverts = (workItems.data ?? []).filter((w) => w.origin === "revert");
  return (
    <Card aria-labelledby={headingId}>
      <header className="p-4">
        <h2 id={headingId} className="text-lg font-semibold break-words">
          {project.name}
        </h2>
        <p className="text-sm break-words text-muted-foreground">
          {t("project.workspace", {
            repo: project.workspace.full_name,
            path: project.workspace.project_path,
            branch: project.workspace.base_branch,
          })}
        </p>
        <Link
          to="/settings/$projectId"
          params={{ projectId: project.id }}
          className="inline-flex min-h-11 items-center text-sm font-medium text-primary underline underline-offset-4"
        >
          {t("project.settings")}
        </Link>
        <Link
          to="/deployments/$projectId"
          params={{ projectId: project.id }}
          className="ml-4 inline-flex min-h-11 items-center text-sm font-medium text-primary underline underline-offset-4"
        >
          {t("project.deployments")}
        </Link>
        <div className="mt-2">
          <QuickDialog project={project} />
        </div>
      </header>
      {workItems.isPending && specs.length > 0 ? (
        <div className="space-y-2 border-t border-border p-4">
          <Skeleton className="h-6" />
          <Skeleton className="h-6" />
        </div>
      ) : (
        <>
          {specs.map((spec) => (
            <SpecGroup
              key={spec.id}
              spec={spec}
              tasks={(workItems.data ?? []).filter(
                (w) => w.spec_id === spec.id,
              )}
              projectTasks={workItems.data ?? []}
            />
          ))}
          <ExternalGroup projectId={project.id} tasks={outside} />
          <RevertGroup projectId={project.id} tasks={reverts} />
          <IssueGroup project={project} />
          {specs.length === 0 &&
            outside.length === 0 &&
            reverts.length === 0 &&
            project.workflow.issue == null && (
              <p className="border-t border-border p-4 text-sm text-muted-foreground">
                {t("delivery.noSpecs")}
              </p>
            )}
        </>
      )}
    </Card>
  );
}
