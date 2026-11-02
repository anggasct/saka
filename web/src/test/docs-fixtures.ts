import { createHash } from "node:crypto";

import type { FileContent, FileTree, FileWrite } from "@/features/docs/queries";

import { blockedTask, PROJECT_ID, spec, task } from "./fixtures";

export const FILES = `/api/v1/projects/${PROJECT_ID}/files`;
export const FILE = `/api/v1/projects/${PROJECT_ID}/file`;
export const SPECS = `/api/v1/projects/${PROJECT_ID}/specs`;
export const WORK_ITEMS = "/api/v1/work-items";
export const TEMPLATE = "/api/v1/spec-template";
export const PROJECTS = "/api/v1/projects";

export const SPEC_PATH = "docs/specs/tasks-done.md";
export const NOTES_PATH = "docs/notes.md";

export function blobOf(content: string): string {
  const bytes = Buffer.from(content, "utf8");
  return createHash("sha1")
    .update(`blob ${bytes.length}\0`)
    .update(bytes)
    .digest("hex");
}

export function hashOf(content: string): string {
  return `sha256:${createHash("sha256").update(content, "utf8").digest("hex")}`;
}

export const SPEC_TEXT = [
  "---",
  "saka: spec",
  "---",
  "# Mark tasks as done",
  "",
  "See [the context](../context.md).",
  "",
  "## Acceptance criteria",
  "",
  "- **AC-01**: A task can be marked done.",
  "",
  "## Steps",
  "",
  "| Step | Work | AC | Done when |",
  "|------|------|----|-----------|",
  "| S1 | Store the flag | AC-01 | `make test` |",
  "| S2 | Show the flag | AC-01 | `make test` |",
  "| S3 | Sync the flag | AC-01 | `make test` |",
  "| S4 | Report the flag | AC-01 | `make test` |",
  "",
].join("\r\n");

export const NOTES_TEXT = [
  "Notes",
  "=====",
  "",
  "## Plan",
  "",
  "```",
  "# not a heading",
  "```",
  "",
  "### Details",
  "",
  "- [ ] check **this**",
  "",
].join("\n");

export function fileTree(extra: Partial<FileTree> = {}): FileTree {
  const files = [
    ["docs/architecture.md", "# Architecture\n"],
    [NOTES_PATH, NOTES_TEXT],
    [SPEC_PATH, SPEC_TEXT],
    ["docs/specs/old/legacy.md", "# Legacy\n"],
  ] as const;
  return {
    commit: "9f1c".padEnd(40, "0"),
    base_branch: "main",
    project_path: "docs",
    files: files.map(([path, content]) => ({
      path,
      blob: blobOf(content),
      bytes: Buffer.byteLength(content),
    })),
    truncated: false,
    ...extra,
  };
}

export function fileContent(path: string, content: string): FileContent {
  return {
    path,
    commit: "9f1c".padEnd(40, "0"),
    blob: blobOf(content),
    bytes: Buffer.byteLength(content),
    content,
  };
}

export function fileWrite(
  path: string,
  content: string,
  extra: Partial<FileWrite> = {},
): FileWrite {
  return {
    path,
    commit: "c0ffee".padEnd(40, "1"),
    blob: blobOf(content),
    bytes: Buffer.byteLength(content),
    committed: true,
    spec: null,
    spec_problem: null,
    ...extra,
  };
}

export function editorSpec(content: string, extra: Partial<typeof spec> = {}) {
  return {
    ...spec,
    path: SPEC_PATH,
    revision: {
      ...spec.revision,
      source_commit: "9f1c".padEnd(40, "0"),
      content_hash: hashOf(content),
    },
    approved_revision: {
      id: spec.revision.id,
      source_commit: "a7e5".padEnd(40, "0"),
      content_hash: hashOf(content),
    },
    lint: { ok: true, issues: [] },
    steps: ["S1", "S2", "S3", "S4"].map((id, i) => ({
      id,
      index: i + 1,
      work: `Work ${id}`,
      ac: ["AC-01"],
      commands: ["make test"],
      after: [],
      work_item_id: task(i + 1, {}).id,
    })),
    ...extra,
  };
}

export const stepTasks = [
  task(1, { state: "queued" }),
  task(2, {
    state: "in_stage",
    stage: "dev",
    stage_phase: "running_session",
  }),
  { ...blockedTask, id: task(3, {}).id, step_id: "S3" },
  task(4, { state: "done" }),
];
