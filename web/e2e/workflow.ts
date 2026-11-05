export const FULL_LOOP = {
  preset: "full_loop",
  spec: true,
  change_request: null,
  stages: [
    { stage: "dev", trigger: "auto" },
    { stage: "review", trigger: "auto" },
    { stage: "fix", trigger: "auto" },
    { stage: "merge", trigger: "manual" },
  ],
};
