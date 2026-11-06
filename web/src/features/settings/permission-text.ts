import type { T } from "@/features/delivery/text";

export function answerSourceText(t: T, source: string) {
  const [kind = "", first = "", ...rest] = source.split(":");
  if (kind === "human") {
    return t("permission.source.human");
  }
  if (kind === "system") {
    return t("permission.source.system");
  }
  if (kind === "policy" && first === "project") {
    return t("permission.source.project");
  }
  if (kind === "policy" && first === "builtin") {
    return t("permission.source.builtin", { name: rest.join(":") });
  }
  if (kind === "policy" && (first === "architect" || first === "reviewer")) {
    return t("permission.source.session", { role: first });
  }
  return t("permission.source.rule", {
    name: [first, ...rest].join(":"),
  });
}
