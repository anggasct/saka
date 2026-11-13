import { useQueryClient } from "@tanstack/react-query";

import { QUERY_ROOTS } from "@/api/events";
import { useT } from "@/i18n/use-t";

import { ActionDialog } from "./action-dialog";
import type { SpecItem, WorkItem } from "./queries";
import { withdrawSpec } from "./queries";
import { RedoDialog } from "./task-actions";

function WithdrawDialog({ spec }: { spec: SpecItem }) {
  const t = useT();
  const queryClient = useQueryClient();
  return (
    <ActionDialog
      trigger={t("spec.withdraw.trigger")}
      title={t("spec.withdraw.title")}
      description={t("spec.withdraw.description")}
      reason={{ label: t("dialog.reason"), required: true }}
      submit={t("spec.withdraw.submit")}
      onSubmit={async (reason, key) => {
        await withdrawSpec(spec.id, reason ?? "", key);
        await Promise.all([
          queryClient.invalidateQueries({ queryKey: [QUERY_ROOTS.specs] }),
          queryClient.invalidateQueries({ queryKey: [QUERY_ROOTS.workItems] }),
        ]);
      }}
    />
  );
}

export function SpecPausedBanner({
  spec,
  tasks,
}: {
  spec: SpecItem;
  tasks: readonly WorkItem[];
}) {
  const t = useT();
  if (spec.state !== "approved") {
    return null;
  }
  const paused = tasks.filter((task) => task.state === "cancelled");
  if (paused.length === 0) {
    return null;
  }
  const steps = paused.map((task) => task.step_id ?? task.task_key).join(", ");
  const redoable = paused.filter((task) =>
    task.available_actions.includes("redo"),
  );
  return (
    <div
      role="status"
      className="mx-4 mt-3 space-y-2 rounded-md border border-border bg-attention p-3 text-sm text-attention-foreground"
    >
      <p>{t("spec.paused", { steps })}</p>
      <div className="flex flex-wrap gap-2">
        {redoable.map((task) => (
          <RedoDialog key={task.id} task={task} named={paused.length > 1} />
        ))}
        <WithdrawDialog spec={spec} />
      </div>
    </div>
  );
}
