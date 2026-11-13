import { useQueryClient } from "@tanstack/react-query";

import { QUERY_ROOTS } from "@/api/events";
import { ActionDialog } from "@/features/delivery/action-dialog";
import { codeOf } from "@/features/delivery/page-error";
import { useT } from "@/i18n/use-t";

import { dismissIncident, draftIncidentFix, revertIncident } from "./queries";

const REFRESH_ON = ["incident_handling", "incident_closed", "invalid_state"];

const ROOTS = [
  QUERY_ROOTS.incidents,
  QUERY_ROOTS.inbox,
  QUERY_ROOTS.workItems,
  QUERY_ROOTS.specDrafts,
];

const NOTE_MAX = 1000;

function useAfterIncidentAction() {
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

export function IncidentDraftFixDialog({
  incidentId,
  primary = false,
}: {
  incidentId: string;
  primary?: boolean;
}) {
  const t = useT();
  const after = useAfterIncidentAction();
  return (
    <ActionDialog
      trigger={t("incident.fix.trigger")}
      triggerVariant={primary ? "default" : "outline"}
      title={t("incident.fix.title")}
      description={t("incident.fix.description")}
      note={{ label: t("incident.fix.note"), max: NOTE_MAX }}
      submit={t("incident.fix.submit")}
      onSubmit={(_, key, note) =>
        after(() => draftIncidentFix(incidentId, note, key))
      }
    />
  );
}

export function IncidentRevertDialog({
  incidentId,
  workItemId,
  taskKey,
}: {
  incidentId: string;
  workItemId: string;
  taskKey: string;
}) {
  const t = useT();
  const after = useAfterIncidentAction();
  return (
    <ActionDialog
      trigger={t("incident.revert.trigger")}
      title={t("incident.revert.title", { task: taskKey })}
      description={t("incident.revert.description", { task: taskKey })}
      reason={{ label: t("dialog.reason"), required: true }}
      submit={t("incident.revert.submit")}
      onSubmit={(reason, key) =>
        after(() => revertIncident(incidentId, workItemId, reason ?? "", key))
      }
    />
  );
}

export function IncidentDismissDialog({ incidentId }: { incidentId: string }) {
  const t = useT();
  const after = useAfterIncidentAction();
  return (
    <ActionDialog
      trigger={t("incident.dismiss.trigger")}
      title={t("incident.dismiss.title")}
      description={t("incident.dismiss.description")}
      reason={{ label: t("dialog.reason"), required: true }}
      submit={t("incident.dismiss.submit")}
      explain={(error) =>
        codeOf(error) === "invalid_state" ? t("incident.dismiss.notOpen") : null
      }
      onSubmit={(reason, key) =>
        after(() => dismissIncident(incidentId, reason ?? "", key))
      }
    />
  );
}
