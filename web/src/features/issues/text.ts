import type { BadgeTone } from "@/ui/badge";

import type { IssueIntake } from "./queries";

const BEFORE_APPROVAL = [
  "accepted",
  "drafting",
  "awaiting_approval",
  "failed",
] as const;

export const MAX_RETRIES = 3;

export function stateTone(state: string): BadgeTone {
  switch (state) {
    case "failed":
      return "danger";
    case "awaiting_approval":
      return "attention";
    case "approved":
    case "in_progress":
      return "progress";
    case "done":
      return "success";
    default:
      return "neutral";
  }
}

export function canDismiss(intake: IssueIntake): boolean {
  return (BEFORE_APPROVAL as readonly string[]).includes(intake.state);
}

export function canRetry(intake: IssueIntake): boolean {
  return intake.state === "failed" && intake.retries < MAX_RETRIES;
}
