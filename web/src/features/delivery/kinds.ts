export const WORK_KINDS = [
  "feature",
  "bugfix",
  "refactor",
  "chore",
  "security_patch",
] as const;

export type WorkKind = (typeof WORK_KINDS)[number];

export const DEFAULT_KIND: WorkKind = "feature";
