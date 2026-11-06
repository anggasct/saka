import { record, text } from "@/features/delivery/text";

import type { DecisionOption } from "./queries";

export function optionsOf(value: unknown): DecisionOption[] {
  if (!Array.isArray(value)) {
    return [];
  }
  return value.map((entry) => {
    const option = record(entry);
    const detail = text(option.detail);
    return { label: text(option.label), detail: detail === "" ? null : detail };
  });
}
