import { ApiError } from "@/api/client";
import { fieldParams } from "@/features/delivery/action-dialog";
import { errorText, label, sha, text, type T } from "@/features/delivery/text";
import type { ChipTone } from "@/editor/chips";

import type { WorkItem } from "./queries";

export function fileErrorText(
  t: T,
  code: string,
  params: Record<string, unknown>,
): string {
  switch (code) {
    case "file_push_rejected":
      return t("error.file_push_rejected", { branch: text(params.branch) });
    case "file_path_conflict":
      return t("error.file_path_conflict", { path: text(params.path) });
    case "file_unsupported":
      return t("error.file_unsupported", {
        reason: text(params.reason) || "other",
      });
    case "spec_slug_conflict":
      return t("error.spec_slug_conflict", { slug: text(params.slug) });
    default:
      return errorText(t, code, fieldParams(params));
  }
}

export function saveErrorText(t: T, error: unknown): string {
  return error instanceof ApiError
    ? fileErrorText(t, error.code, error.params)
    : errorText(t, "network_error");
}

export type ChipView = { text: string; tone: ChipTone };

export function chipOf(t: T, task: WorkItem): ChipView {
  let base: string;
  if (task.state === "in_stage" && task.stage) {
    base = t("docs.chip.stage", {
      stage: label(t, "stage", task.stage),
      phase: label(t, "phase", task.stage_phase ?? ""),
    });
  } else if (task.state === "blocked") {
    base = t("docs.chip.blocked", { code: task.blocker?.code ?? "" });
  } else {
    base = label(t, "task.state", task.state);
  }
  const number = task.change_request?.number;
  const numbered =
    number === undefined ? base : t("docs.chip.change", { text: base, number });
  const revert = task.reverted_by;
  if (revert) {
    const key =
      revert.state === "done" ? "docs.chip.reverted" : "docs.chip.reverting";
    return { text: t(key, { text: numbered }), tone: "blocked" };
  }
  return { text: numbered, tone: toneOf(task.state) };
}

function toneOf(state: string): ChipTone {
  switch (state) {
    case "done":
      return "done";
    case "blocked":
      return "blocked";
    case "admitted":
    case "in_stage":
      return "active";
    default:
      return "waiting";
  }
}

export function shortSha(value: string): string {
  return sha(value);
}
