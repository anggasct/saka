import type { BadgeTone } from "@/ui/badge";

import type { WorkItem } from "./queries";
import { label } from "./text";
import type { T } from "./text";

export function stateTone(state: string): BadgeTone {
  switch (state) {
    case "done":
      return "success";
    case "blocked":
      return "danger";
    case "in_stage":
    case "admitted":
      return "progress";
    default:
      return "neutral";
  }
}

export function placeText(t: T, task: WorkItem): string | null {
  if (task.stage && task.stage_phase) {
    return t("task.place", {
      stage: label(t, "stage", task.stage),
      phase: label(t, "phase", task.stage_phase),
    });
  }
  return task.stage ? label(t, "stage", task.stage) : null;
}

export function isRetired(state: string): boolean {
  return state === "superseded" || state === "cancelled";
}
