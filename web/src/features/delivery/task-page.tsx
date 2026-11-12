import { Link } from "@tanstack/react-router";

import { ApiError } from "@/api/client";
import { useT } from "@/i18n/use-t";
import { Card } from "@/ui/card";
import { Skeleton } from "@/ui/skeleton";

import { BlockerCard } from "./blocker-card";
import { ChangeSection } from "./change-section";
import { EnvironmentSection } from "./environment-section";
import { FixSection } from "./fix-section";
import { GatesSection } from "./gates-section";
import { PageError } from "./page-error";
import {
  useProject,
  useSpec,
  useTaskPermissions,
  useWorkItem,
  useWorkItems,
} from "./queries";
import { RegressionSection } from "./regression-section";
import { ReviewSection } from "./review-section";
import { DecisionSection } from "@/features/decisions/decision-section";

import { SessionSection } from "./session-section";
import { StageRunsSection } from "./stage-runs-section";
import { MergeQueueSection } from "./merge-queue-section";
import { TakenOverBanner } from "./taken-over-banner";
import { TaskActions } from "./task-actions";
import { TaskHeader } from "./task-header";
import { WaitingSection } from "./waiting-section";
import { WorkPacketSection } from "./work-packet-section";

const PAGE = "mx-auto w-full max-w-4xl space-y-4 p-4";

export function TaskPage({ workItemId }: { workItemId: string }) {
  const t = useT();
  const task = useWorkItem(workItemId);
  const spec = useSpec(task.data?.spec_id);
  const project = useProject(task.data?.project_id);
  const siblings = useWorkItems(task.data?.project_id);
  const permissions = useTaskPermissions(workItemId);

  if (task.isPending) {
    return (
      <div
        aria-busy="true"
        aria-label={t("task.loading")}
        role="status"
        className={PAGE}
      >
        <Skeleton className="h-24" />
        <Skeleton className="h-48" />
      </div>
    );
  }
  if (task.isError) {
    if (task.error instanceof ApiError && task.error.status === 404) {
      return (
        <div className={PAGE}>
          <Card className="space-y-3 p-4">
            <h1 className="text-xl font-semibold">{t("task.notFound")}</h1>
            <Link
              to="/delivery"
              className="inline-flex min-h-11 items-center text-sm font-medium text-primary underline underline-offset-4"
            >
              {t("task.back")}
            </Link>
          </Card>
        </div>
      );
    }
    return (
      <div className={PAGE}>
        <PageError
          title="task.error.title"
          error={task.error}
          onRetry={() => {
            void task.refetch();
          }}
        />
      </div>
    );
  }
  const item = task.data;
  const tasks = siblings.data ?? [];
  return (
    <div className={PAGE}>
      <TaskHeader
        task={item}
        spec={spec.data}
        project={project.data}
        firstPermission={permissions.data?.[0]?.id}
        actions={<TaskActions task={item} />}
      />
      {item.blocker && (
        <BlockerCard
          task={item}
          blocker={item.blocker}
          specPath={spec.data?.path}
        />
      )}
      <TakenOverBanner task={item} />
      <WaitingSection task={item} tasks={tasks} />
      <MergeQueueSection task={item} />
      <ChangeSection task={item} />
      <GatesSection task={item} />
      <ReviewSection task={item} />
      <FixSection task={item} />
      <RegressionSection task={item} />
      <StageRunsSection task={item} spec={spec.data} tasks={tasks} />
      <EnvironmentSection task={item} />
      <WorkPacketSection workItemId={item.id} />
      <DecisionSection run={{ kind: "work_item", id: item.id }} />
      <SessionSection
        owner={{ kind: "work_item", id: item.id }}
        title={t("task.session.title")}
        empty={t("task.session.none")}
        project={project.data?.name}
        permissions={permissions.data ?? []}
      />
    </div>
  );
}
