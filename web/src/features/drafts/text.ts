import type { WaitingOn } from "@/features/delivery/queries";
import {
  label,
  record,
  sha,
  text,
  type Relative,
  type T,
} from "@/features/delivery/text";
import { CATALOGS, type MessageKey } from "@/i18n/messages";
import type { BadgeTone } from "@/ui/badge";

import type { SpecDraft } from "./queries";

const NONE = "none";

function isKey(key: string): key is MessageKey {
  return key in CATALOGS.en;
}

export const ACTIVE_STATES = [
  "queued",
  "preparing",
  "drafting",
  "awaiting_operator",
  "committing",
  "failed",
] as const;

export function isActive(draft: SpecDraft): boolean {
  return (ACTIVE_STATES as readonly string[]).includes(draft.state);
}

export function stateText(t: T, state: string): string {
  return label(t, "draft.state", state);
}

export function stateTone(state: string): BadgeTone {
  switch (state) {
    case "succeeded":
      return "success";
    case "failed":
      return "danger";
    case "cancelled":
      return "neutral";
    case "queued":
      return "attention";
    default:
      return "progress";
  }
}

export function failureText(
  t: T,
  code: string,
  params: Record<string, unknown>,
  relative: Relative,
): string {
  const key = `draft.failure.${code}`;
  if (!isKey(key)) {
    return t("draft.failure.unknown", { code });
  }
  const error = record(params.error);
  const resetsAt = text(params.resets_at);
  const values: Record<string, string | number> = {
    rounds: Number(text(params.rounds)) || 0,
    dimension: text(params.dimension) || "other",
    used: text(params.used),
    limit: text(params.limit),
    account: text(params.account),
    window: text(params.window),
    resets: resetsAt === "" ? NONE : relative(resetsAt),
    cause: text(params.cause) || "other",
    reason: text(params.reason),
    minutes: Number(text(params.minutes)) || 0,
    hours: Number(text(params.hours)) || 0,
    tool: text(params.tool),
    summary: text(params.summary),
    question: text(params.question),
    error: text(error.code) || "other",
    path: text(params.path),
    phase: text(params.phase) || "other",
    branch: text(params.branch),
    blob: sha(params.blob),
    code: text(params.code) || "other",
  };
  return t(key, values);
}

export function waitingText(
  t: T,
  item: WaitingOn,
  relative: Relative,
  percent: (value: number) => string,
): string {
  switch (item.kind) {
    case "quota":
      return t("draft.waiting.quota", {
        account: item.account,
        window: item.window,
        percent: percent(item.utilization),
        resets:
          item.resets_at !== null && item.resets_at !== undefined
            ? relative(item.resets_at)
            : NONE,
      });
    case "architect_slot":
      return t("draft.waiting.architect_slot");
    case "step":
    case "spec":
    case "port_block":
      return t("draft.queued.body");
  }
}

export function roundText(t: T, draft: SpecDraft): string | null {
  const attempt = draft.attempt;
  return attempt === null || attempt === undefined
    ? null
    : t("draft.round", { round: attempt.rounds, max: attempt.max_rounds });
}
