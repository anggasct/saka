import { useQueryClient } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { useState } from "react";

import { newIdempotencyKey } from "@/api/client";
import { QUERY_ROOTS } from "@/api/events";
import { ActionDialog } from "@/features/delivery/action-dialog";
import { codeOf } from "@/features/delivery/page-error";
import {
  answerPermission,
  mergeTask,
  runPrimaryAction,
  startTask,
} from "@/features/delivery/queries";
import type { T } from "@/features/delivery/text";
import { errorText, label, record, text } from "@/features/delivery/text";
import { useT } from "@/i18n/use-t";
import { Button } from "@/ui/button";

import {
  dismissIssueIntake,
  retryIssueIntake,
} from "@/features/issues/queries";

import type { InboxItem } from "./queries";
import { approveSpec, dismissItem } from "./queries";

const LINK =
  "inline-flex min-h-11 items-center rounded-md px-4 text-sm font-medium";
const PRIMARY_LINK = `${LINK} bg-primary text-primary-foreground`;
const QUIET_LINK = `${LINK} border border-border bg-background hover:bg-muted`;

const RELOAD_ON = [
  "version_stale",
  "revision_stale",
  "invalid_state",
  "blocker_not_open",
  "blocker_action_changed",
  "permission_already_answered",
  "inbox_item_closed",
];

const ROOTS = [
  QUERY_ROOTS.inbox,
  QUERY_ROOTS.workItems,
  QUERY_ROOTS.permissions,
  QUERY_ROOTS.specs,
  QUERY_ROOTS.sessions,
  QUERY_ROOTS.issueIntakes,
  QUERY_ROOTS.specDrafts,
];

function useAfterAction() {
  const queryClient = useQueryClient();
  const refresh = () =>
    Promise.all(
      ROOTS.map((root) => queryClient.invalidateQueries({ queryKey: [root] })),
    );
  return async (action: () => Promise<void>) => {
    try {
      await action();
    } catch (e) {
      if (RELOAD_ON.includes(codeOf(e))) {
        await refresh();
      }
      throw e;
    }
    await refresh();
  };
}

function Permission({ item }: { item: InboxItem }) {
  const t = useT();
  const after = useAfterAction();
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const id = text(record(item.action.params).permission_request_id);
  const tool = text(record(item.params).tool);

  const allow = async () => {
    setPending(true);
    setError(null);
    try {
      await after(() =>
        answerPermission(
          id,
          { decision: "allow", message: null, remember: false },
          newIdempotencyKey(),
        ),
      );
    } catch (e) {
      setError(e);
    } finally {
      setPending(false);
    }
  };

  return (
    <>
      <Button
        disabled={pending}
        onClick={() => {
          void allow();
        }}
      >
        {t("permission.allow")}
      </Button>
      <ActionDialog
        trigger={t("permission.deny.trigger")}
        title={t("permission.deny.title")}
        description={t("permission.deny.description", { tool })}
        reason={{ label: t("permission.deny.message"), required: false }}
        submit={t("permission.deny.submit")}
        onSubmit={(message, key) =>
          after(() =>
            answerPermission(
              id,
              { decision: "deny", message, remember: false },
              key,
            ),
          )
        }
      />
      {error !== null && (
        <p role="alert" className="w-full text-danger-foreground">
          {errorText(t, codeOf(error))}
        </p>
      )}
    </>
  );
}

type BlockerKind = "retry" | "cancel" | "takeover" | "update_branch";

function blockerCopy(t: T, kind: BlockerKind) {
  switch (kind) {
    case "retry":
      return {
        trigger: t("task.retry.trigger"),
        title: t("task.retry.title"),
        description: t("task.retry.description"),
        submit: t("task.retry.submit"),
      };
    case "cancel":
      return {
        trigger: t("task.cancel.trigger"),
        title: t("task.cancel.title"),
        description: t("task.cancel.description"),
        submit: t("task.cancel.submit"),
      };
    case "takeover":
      return {
        trigger: t("task.takeover.trigger"),
        title: t("task.takeover.title"),
        description: t("task.takeover.description"),
        submit: t("task.takeover.submit"),
      };
    case "update_branch":
      return {
        trigger: t("task.update_branch.trigger"),
        title: t("task.update_branch.title"),
        description: t("task.update_branch.description"),
        submit: t("task.update_branch.submit"),
      };
  }
}

function Blocker({ item, kind }: { item: InboxItem; kind: BlockerKind }) {
  const t = useT();
  const after = useAfterAction();
  const blockerId = text(record(item.action.params).blocker_id);
  const workItemId = item.work_item_id ?? "";
  const copy = blockerCopy(t, kind);
  return (
    <ActionDialog
      trigger={copy.trigger}
      triggerVariant="default"
      title={copy.title}
      description={copy.description}
      reason={{ label: t("dialog.reason"), required: true }}
      submit={copy.submit}
      onSubmit={(reason, key) =>
        after(() => runPrimaryAction(workItemId, blockerId, reason ?? "", key))
      }
    />
  );
}

function Approve({ item }: { item: InboxItem }) {
  const t = useT();
  const after = useAfterAction();
  const params = record(item.action.params);
  return (
    <ActionDialog
      trigger={t("inbox.approve.trigger")}
      triggerVariant="default"
      title={t("inbox.approve.title")}
      description={t("inbox.approve.description")}
      reason={{ label: t("dialog.reasonOptional"), required: false }}
      submit={t("inbox.approve.submit")}
      onSubmit={(reason, key) =>
        after(() =>
          approveSpec(text(params.spec_id), text(params.revision), reason, key),
        )
      }
    />
  );
}

function IntakeRetry({ item }: { item: InboxItem }) {
  const t = useT();
  const after = useAfterAction();
  const id = text(record(item.action.params).intake_id);
  return (
    <ActionDialog
      trigger={t("issues.retry.trigger")}
      triggerVariant="default"
      title={t("issues.retry.title")}
      description={t("issues.retry.description", {
        left: 3 - Number(record(item.params).retries),
      })}
      submit={t("issues.retry.submit")}
      onSubmit={(_, key) => after(() => retryIssueIntake(id, key))}
    />
  );
}

function IntakeDismiss({
  item,
  primary,
}: {
  item: InboxItem;
  primary: boolean;
}) {
  const t = useT();
  const after = useAfterAction();
  const id = text(record(item.action.params).intake_id);
  return (
    <ActionDialog
      trigger={t("issues.dismiss.trigger")}
      triggerVariant={primary ? "default" : "outline"}
      title={t("issues.dismiss.title")}
      description={t("issues.dismiss.description")}
      reason={{ label: t("issues.dismiss.reason"), required: false }}
      submit={t("issues.dismiss.submit")}
      onSubmit={(reason, key) =>
        after(() => dismissIssueIntake(id, reason, key))
      }
    />
  );
}

function Start({ item }: { item: InboxItem }) {
  const t = useT();
  const after = useAfterAction();
  const params = record(item.action.params);
  return (
    <ActionDialog
      trigger={t("task.start.trigger")}
      triggerVariant="default"
      title={t("task.start.title")}
      description={t("task.start.description")}
      submit={t("task.start.submit")}
      onSubmit={(_, key) =>
        after(() =>
          startTask(text(params.work_item_id), Number(params.version), key),
        )
      }
    />
  );
}

function Merge({ item }: { item: InboxItem }) {
  const t = useT();
  const after = useAfterAction();
  const params = record(item.action.params);
  return (
    <ActionDialog
      trigger={t("task.merge.trigger")}
      triggerVariant="default"
      title={t("task.merge.title")}
      description={t("task.merge.description")}
      reason={{ label: t("dialog.reasonOptional"), required: false }}
      submit={t("task.merge.submit")}
      onSubmit={(reason, key) =>
        after(() =>
          mergeTask(
            text(params.work_item_id),
            Number(params.version),
            reason,
            key,
          ),
        )
      }
    />
  );
}

export function Primary({ item }: { item: InboxItem }) {
  const t = useT();
  const kind = item.action.kind;
  switch (kind) {
    case "answer_permission":
      return <Permission item={item} />;
    case "approve_spec":
      return <Approve item={item} />;
    case "start_step":
      return <Start item={item} />;
    case "merge":
      return <Merge item={item} />;
    case "retry_intake":
      return <IntakeRetry item={item} />;
    case "dismiss_intake":
      return <IntakeDismiss item={item} primary />;
    case "retry":
    case "cancel":
    case "takeover":
    case "update_branch":
      return <Blocker item={item} kind={kind} />;
    case "edit_spec":
      return item.title.spec_path == null ? null : (
        <Link
          to="/docs/$projectId"
          params={{ projectId: item.project_id }}
          search={{ path: item.title.spec_path }}
          className={PRIMARY_LINK}
        >
          {label(t, "blocker.action", kind)}
        </Link>
      );
    case "set_secret":
    case "link_account":
    case "set_user_token":
      return (
        <Link
          to="/settings/$projectId"
          params={{ projectId: item.project_id }}
          className={PRIMARY_LINK}
        >
          {label(t, "blocker.action", kind)}
        </Link>
      );
    default:
      return null;
  }
}

export function Secondary({ item }: { item: InboxItem }) {
  const t = useT();
  const after = useAfterAction();
  return (
    <>
      {item.work_item_id != null && (
        <Link
          to="/delivery/tasks/$workItemId"
          params={{ workItemId: item.work_item_id }}
          className={QUIET_LINK}
        >
          {t("inbox.open.task")}
        </Link>
      )}
      {item.work_item_id == null && item.title.spec_path != null && (
        <Link
          to="/docs/$projectId"
          params={{ projectId: item.project_id }}
          search={{ path: item.title.spec_path }}
          className={QUIET_LINK}
        >
          {t("inbox.open.spec")}
        </Link>
      )}
      {item.action.kind === "retry_intake" && (
        <IntakeDismiss item={item} primary={false} />
      )}
      {item.dismissible && (
        <ActionDialog
          trigger={t("inbox.dismiss.trigger")}
          title={t("inbox.dismiss.title")}
          description={t("inbox.dismiss.description")}
          reason={{ label: t("dialog.reason"), required: true }}
          submit={t("inbox.dismiss.submit")}
          onSubmit={(reason, key) =>
            after(() => dismissItem(item.id, reason ?? "", key))
          }
        />
      )}
    </>
  );
}
