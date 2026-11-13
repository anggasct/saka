import { useQueryClient } from "@tanstack/react-query";

import { QUERY_ROOTS } from "@/api/events";
import { ActionDialog } from "@/features/delivery/action-dialog";
import { codeOf } from "@/features/delivery/page-error";
import { useT } from "@/i18n/use-t";

import { cancelDeployment, draftFix, redeployEnvironment } from "./queries";

const REFRESH_ON = [
  "deployment_queued",
  "deployment_running",
  "deployment_finished",
  "deployment_not_failed",
  "fix_draft_in_progress",
];

const ROOTS = [
  QUERY_ROOTS.deployments,
  QUERY_ROOTS.workItems,
  QUERY_ROOTS.inbox,
  QUERY_ROOTS.specDrafts,
];

function useAfterDeployAction() {
  const queryClient = useQueryClient();
  const refresh = () =>
    Promise.all(
      ROOTS.map((root) => queryClient.invalidateQueries({ queryKey: [root] })),
    );
  return async (action: () => Promise<void>) => {
    try {
      await action();
    } catch (e) {
      if (REFRESH_ON.includes(codeOf(e))) {
        await refresh();
      }
      throw e;
    }
    await refresh();
  };
}

export function RedeployDialog({
  projectId,
  environment,
  primary = false,
}: {
  projectId: string;
  environment: string;
  primary?: boolean;
}) {
  const t = useT();
  const after = useAfterDeployAction();
  return (
    <ActionDialog
      trigger={t("deploy.redeploy.trigger")}
      triggerVariant={primary ? "default" : "outline"}
      title={t("deploy.redeploy.title", { environment })}
      description={t("deploy.redeploy.description", { environment })}
      reason={{ label: t("dialog.reason"), required: true }}
      submit={t("deploy.redeploy.submit")}
      onSubmit={(reason, key) =>
        after(() =>
          redeployEnvironment(projectId, environment, reason ?? "", key),
        )
      }
    />
  );
}

export function CancelDeploymentDialog({
  deploymentId,
}: {
  deploymentId: string;
}) {
  const t = useT();
  const after = useAfterDeployAction();
  return (
    <ActionDialog
      trigger={t("deploy.cancel.trigger")}
      title={t("deploy.cancel.title")}
      description={t("deploy.cancel.description")}
      reason={{ label: t("dialog.reason"), required: true }}
      submit={t("deploy.cancel.submit")}
      onSubmit={(reason, key) =>
        after(() => cancelDeployment(deploymentId, reason ?? "", key))
      }
    />
  );
}

const NOTE_MAX = 1000;

export function DraftFixDialog({
  deploymentId,
  primary = false,
}: {
  deploymentId: string;
  primary?: boolean;
}) {
  const t = useT();
  const after = useAfterDeployAction();
  return (
    <ActionDialog
      trigger={t("deploy.fix.trigger")}
      triggerVariant={primary ? "default" : "outline"}
      title={t("deploy.fix.dialogTitle")}
      description={t("deploy.fix.description")}
      note={{ label: t("deploy.fix.note"), max: NOTE_MAX }}
      submit={t("deploy.fix.submit")}
      onSubmit={(_, key, note) =>
        after(() => draftFix(deploymentId, note, key))
      }
    />
  );
}
