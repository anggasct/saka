import type { BadgeTone } from "@/ui/badge";

export function severityTone(severity: string): BadgeTone {
  switch (severity) {
    case "critical":
    case "high":
      return "danger";
    case "medium":
      return "attention";
    default:
      return "neutral";
  }
}

export function stateTone(state: string): BadgeTone {
  switch (state) {
    case "open":
      return "danger";
    case "handling":
      return "progress";
    case "resolved":
      return "success";
    default:
      return "neutral";
  }
}

export function httpUrl(value: string): string | null {
  try {
    const { protocol } = new URL(value);
    return protocol === "http:" || protocol === "https:" ? value : null;
  } catch {
    return null;
  }
}

export function labelEntries(
  labels: Readonly<Record<string, string>>,
): [string, string][] {
  return Object.entries(labels).toSorted(([a], [b]) => a.localeCompare(b));
}
