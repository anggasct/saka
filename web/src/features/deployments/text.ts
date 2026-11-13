import { record, text } from "@/features/delivery/text";
import type { T } from "@/features/delivery/text";
import { CATALOGS } from "@/i18n/messages";
import type { MessageKey } from "@/i18n/messages";
import type { BadgeTone } from "@/ui/badge";

import type { Deployment } from "./queries";

const ERROR_PARAMS: Record<string, readonly string[]> = {
  deploy_command_failed: ["exit_code"],
  deploy_webhook_rejected: ["status"],
  deploy_secret_missing: ["name"],
  ready_timed_out: ["attempts"],
  verify_failed: ["failed", "total"],
};

const CANCELLABLE = ["queued", "awaiting_ready", "verifying"];

function known(key: string): key is MessageKey {
  return key in CATALOGS.en;
}

function numeric(value: unknown): number {
  return typeof value === "number" ? value : 0;
}

export function stateTone(state: string): BadgeTone {
  switch (state) {
    case "succeeded":
      return "success";
    case "failed":
      return "danger";
    case "queued":
      return "attention";
    case "cancelled":
      return "neutral";
    default:
      return "progress";
  }
}

export function gateTone(state: string): BadgeTone {
  switch (state) {
    case "passed":
      return "success";
    case "failed":
      return "danger";
    case "accepted":
      return "attention";
    case "pending":
      return "progress";
    default:
      return "neutral";
  }
}

export function checkTone(state: string): BadgeTone {
  switch (state) {
    case "passed":
      return "success";
    case "failed":
    case "timed_out":
      return "danger";
    default:
      return "neutral";
  }
}

export function isCancellable(deployment: Deployment): boolean {
  return CANCELLABLE.includes(deployment.state);
}

export function gateReason(t: T, state: string, reason: string): string {
  const key = `deploy.gateReason.${reason}`;
  return state === "skipped" && known(key) ? t(key) : reason;
}

export function deployErrorText(
  t: T,
  error: { code: string; params?: unknown },
): string {
  const key = `deploy.error.${error.code}`;
  if (!known(key)) {
    return t("deploy.error.unknown", { code: error.code });
  }
  const params = record(error.params);
  const needed = ERROR_PARAMS[error.code] ?? [];
  return t(key, {
    known: needed.every((name) => params[name] !== undefined) ? "yes" : "no",
    exit_code: text(params.exit_code),
    status: text(params.status),
    name: text(params.name),
    attempts: numeric(params.attempts),
    failed: numeric(params.failed),
    total: numeric(params.total),
  });
}
