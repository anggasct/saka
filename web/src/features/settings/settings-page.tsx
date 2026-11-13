import { Link } from "@tanstack/react-router";

import { ApiError } from "@/api/client";
import { PageError } from "@/features/delivery/page-error";
import { useProject } from "@/features/delivery/queries";
import { useT } from "@/i18n/use-t";
import { Card } from "@/ui/card";
import { Skeleton } from "@/ui/skeleton";

import { AuthorshipSection } from "./authorship-section";
import { ConfigSection } from "./config-section";
import { DiagnosticsSection } from "./diagnostics-section";
import { EnvironmentsSection } from "./environments-section";
import { HooksSection } from "./hooks-section";
import { PermissionsSection } from "./permissions-section";
import { ReposSection } from "./repos-section";
import { SecretsSection } from "./secrets-section";
import { WorkflowSection } from "./workflow-section";

export function SettingsPage({ projectId }: { projectId: string }) {
  const t = useT();
  const project = useProject(projectId);
  const missing =
    project.error instanceof ApiError && project.error.status === 404;
  return (
    <div className="mx-auto w-full max-w-4xl space-y-4 p-4">
      {project.data !== undefined ? (
        <>
          <h1 className="text-2xl font-semibold break-words">
            {t("settings.project.title", { name: project.data.name })}
          </h1>
          <ReposSection project={project.data} />
          <ConfigSection project={project.data} />
          <WorkflowSection project={project.data} />
          <EnvironmentsSection projectId={project.data.id} />
          <HooksSection
            projectId={project.data.id}
            skipped={project.data.hooks_skipped}
          />
          <PermissionsSection projectId={project.data.id} />
          <SecretsSection projectId={project.data.id} />
          <AuthorshipSection projectId={project.data.id} />
          <DiagnosticsSection projectId={project.data.id} />
        </>
      ) : missing ? (
        <Card role="alert" className="space-y-3 p-4">
          <h1 className="text-lg font-semibold">
            {t("settings.notFound.title")}
          </h1>
          <Link
            to="/settings"
            className="inline-flex min-h-11 items-center text-sm font-medium text-primary underline underline-offset-4"
          >
            {t("settings.notFound.back")}
          </Link>
        </Card>
      ) : project.error !== null ? (
        <PageError
          title="settings.error.title"
          error={project.error}
          onRetry={() => void project.refetch()}
        />
      ) : (
        <div
          role="status"
          aria-busy="true"
          aria-label={t("settings.loading")}
          className="space-y-3"
        >
          <Skeleton className="h-8 w-1/2" />
          <Skeleton className="h-40" />
          <Skeleton className="h-24" />
        </div>
      )}
    </div>
  );
}
