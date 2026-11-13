import { Link } from "@tanstack/react-router";

import { DraftFixDialog } from "@/features/deployments/actions";
import { useT } from "@/i18n/use-t";

import type { WorkItem } from "./queries";
import {
  CancelDialog,
  RerunChecksDialog,
  RetryDialog,
  RevertDialog,
  TakeoverDialog,
  UpdateBranchDialog,
} from "./task-actions";
import { label, record, text } from "./text";

const LINK =
  "inline-flex min-h-11 items-center rounded-md bg-primary px-4 text-sm font-medium text-primary-foreground";

export function PrimaryAction({
  task,
  specPath,
}: {
  task: WorkItem;
  specPath: string | undefined;
}) {
  const t = useT();
  const kind = task.blocker?.primary_action.kind;
  switch (kind) {
    case undefined:
      return null;
    case "retry":
      return <RetryDialog task={task} primary />;
    case "cancel":
      return <CancelDialog task={task} primary />;
    case "takeover":
      return <TakeoverDialog task={task} primary />;
    case "rerun_checks":
      return <RerunChecksDialog task={task} primary />;
    case "update_branch":
      return <UpdateBranchDialog task={task} />;
    case "revert":
      return <RevertDialog task={task} primary />;
    case "draft_fix":
      return (
        <DraftFixDialog
          deploymentId={text(
            record(task.blocker?.primary_action.params).deployment_id,
          )}
          primary
        />
      );
    case "edit_spec":
      return specPath === undefined ? null : (
        <Link
          to="/docs/$projectId"
          params={{ projectId: task.project_id }}
          search={{ path: specPath }}
          className={LINK}
        >
          {label(t, "blocker.action", kind)}
        </Link>
      );
    default:
      return (
        <Link
          to="/settings/$projectId"
          params={{ projectId: task.project_id }}
          className={LINK}
        >
          {label(t, "blocker.action", kind)}
        </Link>
      );
  }
}
