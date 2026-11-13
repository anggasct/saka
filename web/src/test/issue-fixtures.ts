import type { IssueIntake } from "@/features/issues/queries";
import type { ProjectItem } from "@/features/delivery/queries";

import { PROJECT_ID, SPEC_ID, project } from "./fixtures";

export const REPO_ID = "0193f0c4-1b2a-7c3d-9e8f-001122334400";

export const issueProject: ProjectItem = {
  ...project,
  workflow: {
    ...project.workflow,
    issue: {
      trigger: "label",
      trigger_label: "saka:intake",
      labels: [],
      exclude_labels: ["wontfix", "duplicate"],
      kind_labels: { bug: "bugfix", chore: "chore" },
      default_kind: "feature",
      include_confidential: false,
      max_open: 20,
      bugfix: {
        done_when: ["cargo test --locked"],
        regression: ["cargo test --locked crash"],
      },
    },
  },
};

let next = 0;

export function intake(overrides: Partial<IssueIntake> = {}): IssueIntake {
  next += 1;
  const n = String(next).padStart(2, "0");
  return {
    id: `0193f0c4-1b2a-7c3d-9e8f-0011223388${n}`,
    project_id: PROJECT_ID,
    repo: { id: REPO_ID, name: "app", full_name: "acme/tasks" },
    issue: {
      number: 40 + next,
      url: `https://github.com/acme/tasks/issues/${40 + next}`,
      title: "Crash on start",
      author: "alice",
    },
    state: "awaiting_approval",
    lane: "bugfix",
    kind: "bugfix",
    cause: "label",
    actor: "maintainer",
    spec: {
      id: SPEC_ID,
      path: "docs/specs/bugfix-crash-on-start-41.md",
      state: "ready",
    },
    draft: null,
    failure: null,
    retries: 0,
    created_at: "2026-11-13T10:00:00.000Z",
    updated_at: "2026-11-13T10:00:00.000Z",
    ...overrides,
  };
}

export const waitingIntake = intake();

export const draftingIntake = intake({
  state: "drafting",
  lane: "draft",
  kind: "chore",
  spec: null,
  draft: {
    id: "0193f0c4-1b2a-7c3d-9e8f-001122337701",
    state: "drafting",
  },
});

export const failedIntake = intake({
  state: "failed",
  lane: "draft",
  kind: "feature",
  spec: null,
  draft: null,
  failure: { code: "draft_failed", params: {} },
  retries: 1,
});

export const exhaustedIntake = intake({
  state: "failed",
  lane: "draft",
  kind: "feature",
  spec: null,
  failure: { code: "draft_failed", params: {} },
  retries: 3,
});

export const doneIntake = intake({ state: "done" });
