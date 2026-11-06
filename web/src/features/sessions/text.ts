import type { BadgeTone } from "@/ui/badge";

export function sessionTone(state: string): BadgeTone {
  switch (state) {
    case "running":
    case "resumed":
      return "progress";
    case "waiting":
      return "attention";
    case "lost":
    case "failed":
      return "danger";
    default:
      return "neutral";
  }
}
