import { createIntl } from "react-intl";
import { describe, expect, it } from "vitest";

import type { Locale, MessageKey } from "@/i18n/messages";
import { CATALOGS } from "@/i18n/messages";

import { blockerText, errorText, label } from "./text";

function translator(locale: Locale) {
  const intl = createIntl({ locale, messages: CATALOGS[locale] });
  return (key: MessageKey, values?: Record<string, string | number>) =>
    intl.formatMessage({ id: key }, values);
}

describe("blockerText", () => {
  const en = translator("en");
  const id = translator("id");

  it("names the cause of a failed or timed out session", () => {
    expect(
      blockerText(en, "session_failed", {
        cause: "operator_stop",
        reason: "wrong file",
      }),
    ).toBe("The operator stopped the agent session: wrong file");
    expect(blockerText(en, "session_failed", { subtype: "error" })).toBe(
      "The agent session failed.",
    );
    expect(
      blockerText(en, "session_timeout", { cause: "idle", minutes: 30 }),
    ).toBe("The interrupted agent waited more than 30 minutes for a steer.");
    expect(
      blockerText(id, "session_failed", {
        cause: "operator_stop",
        reason: "salah file",
      }),
    ).toBe("Operator menghentikan sesi agent: salah file");
    expect(
      blockerText(id, "session_timeout", { cause: "idle", minutes: 30 }),
    ).toBe("Agent yang di-interrupt menunggu arahan lebih dari 30 menit.");
  });

  it("explains expired permissions, budgets, and quotas", () => {
    expect(
      blockerText(en, "permission_expired", {
        tool: "Bash",
        summary: "npm install",
        hours: 24,
      }),
    ).toBe(
      "A Bash request waited more than 24 hours for an answer: npm install",
    );
    expect(
      blockerText(en, "budget_exhausted", {
        dimension: "turns",
        used: 10,
        limit: 10,
      }),
    ).toBe("The stage used its 10 agent turns.");
    expect(
      blockerText(en, "budget_exhausted", {
        dimension: "cost_usd",
        used: "1.200000",
        limit: "1.000000",
      }),
    ).toBe("The stage cost 1.200000 of 1.000000 USD.");
    expect(
      blockerText(en, "quota_exhausted", {
        account: "claude:default",
        window: "five_hour",
        resets_at: null,
      }),
    ).toBe("The agent account claude:default hit its five_hour rate limit.");
    expect(
      blockerText(id, "budget_exhausted", {
        dimension: "running_minutes",
        used: 60,
        limit: 60,
      }),
    ).toBe("Stage memakai habis 60 menit waktu running.");
    expect(
      blockerText(id, "quota_exhausted", {
        account: "claude:default",
        window: "seven_day",
      }),
    ).toBe("Akun agent claude:default mencapai batas rate seven_day.");
  });

  it("points at the failing recipe command or recipe line", () => {
    const failed = {
      error: {
        code: "setup_command_failed",
        params: { command_index: 1, exit_code: 2, log: "/l/setup-1.log" },
      },
    };
    expect(blockerText(en, "env_setup_failed", failed)).toBe(
      "Setup command 2 of the environment recipe failed.",
    );
    expect(blockerText(id, "env_setup_failed", failed)).toBe(
      "Perintah setup ke-2 dari resep environment gagal.",
    );
    expect(
      blockerText(en, "env_setup_failed", {
        error: {
          code: "setup_command_failed",
          params: { command_index: 0, timed_out: true },
        },
      }),
    ).toBe("Setup command 1 of the environment recipe ran out of time.");
    expect(
      blockerText(en, "env_setup_failed", {
        error: { code: "worktree_add_failed", params: {} },
      }),
    ).toBe("The task environment could not be prepared.");
    expect(
      blockerText(en, "env_recipe_invalid", {
        code: "env_name_reserved",
        field: "env.PATH",
        line: 4,
      }),
    ).toBe(
      "The environment recipe .saka/environment.toml was refused: env_name_reserved env.PATH (line 4).",
    );
    expect(
      blockerText(id, "env_recipe_invalid", {
        code: "env_recipe_too_large",
        field: null,
        line: null,
      }),
    ).toBe(
      "Resep environment .saka/environment.toml ditolak: env_recipe_too_large.",
    );
  });

  it("says which toolchain is unpinned or unavailable", () => {
    expect(
      blockerText(en, "toolchain_unpinned", {
        tool: "node",
        source: "nvmrc",
        value: "lts/*",
      }),
    ).toBe(
      "The repo asks for node lts/*; pin an exact version (for example 1.2.3) in nvmrc.",
    );
    expect(
      blockerText(id, "toolchain_unpinned", {
        tool: "node",
        source: "pnpm_requires_node",
        value: null,
      }),
    ).toBe(
      "pnpm butuh versi Node yang dipin; deklarasikan di package.json devEngines, .node-version, atau .nvmrc.",
    );
    expect(
      blockerText(en, "toolchain_unavailable", {
        tool: "go",
        version: "1.26.3",
        cause: "checksum_mismatch",
      }),
    ).toBe("The go 1.26.3 download did not match its published checksum.");
    expect(
      blockerText(id, "toolchain_unavailable", {
        tool: "pnpm",
        version: "12.16.1",
        cause: "download_failed",
      }),
    ).toBe("pnpm 12.16.1 tidak bisa diunduh.");
  });

  it("names the secrets a recipe is missing or cannot open", () => {
    expect(
      blockerText(en, "secret_missing", { names: ["NPM_TOKEN", "API_KEY"] }),
    ).toBe(
      "The environment recipe asks for secrets the project does not have: NPM_TOKEN, API_KEY. Set them with saka secret set, then retry.",
    );
    expect(blockerText(id, "secret_unreadable", { name: "NPM_TOKEN" })).toBe(
      "Secret NPM_TOKEN tidak bisa didekripsi dengan key server ini; simpan ulang, lalu coba lagi.",
    );
  });
});

const HEAD = "a1b2c3d4e5f6".padEnd(40, "0");

const VARIANTS: [string, Record<string, unknown>][] = [
  [
    "env_setup_failed",
    {
      error: {
        code: "setup_command_failed",
        params: { command_index: 0, exit_code: 1 },
      },
    },
  ],
  [
    "env_setup_failed",
    {
      error: {
        code: "setup_command_failed",
        params: { command_index: 0, timed_out: true },
      },
    },
  ],
  ["env_setup_failed", { error: { code: "port_block_unavailable" } }],
  ["env_setup_failed", { error: { code: "worktree_add_failed" } }],
  ["env_recipe_invalid", { code: "env_recipe_too_large", line: 3 }],
  ["toolchain_unpinned", { tool: "go", source: "go.mod", value: "1.26" }],
  ["toolchain_unpinned", { source: "pnpm_requires_node" }],
  ["toolchain_unavailable", { tool: "go", version: "1", cause: "not_found" }],
  [
    "toolchain_unavailable",
    { tool: "go", version: "1", cause: "checksum_mismatch" },
  ],
  [
    "toolchain_unavailable",
    { tool: "go", version: "1", cause: "download_failed" },
  ],
  [
    "toolchain_unavailable",
    { tool: "go", version: "1", cause: "unsupported_platform" },
  ],
  [
    "toolchain_unavailable",
    { tool: "go", version: "1", cause: "archive_invalid" },
  ],
  ["secret_missing", { names: ["A"] }],
  ["secret_unreadable", { name: "A" }],
  ["session_failed", { cause: "operator_stop", reason: "stop" }],
  ["session_failed", { subtype: "error_during_execution" }],
  ["session_lost", { cause: "runner_silent" }],
  ["session_timeout", { cause: "idle", minutes: 30 }],
  ["session_timeout", { minutes: 60 }],
  ["permission_expired", { tool: "Bash", summary: "ls", hours: 24 }],
  ["budget_exhausted", { dimension: "running_minutes", used: 60, limit: 60 }],
  ["budget_exhausted", { dimension: "turns", used: 10, limit: 10 }],
  ["budget_exhausted", { dimension: "tokens", used: 9, limit: 9 }],
  ["budget_exhausted", { dimension: "cost_usd", used: "1", limit: "1" }],
  ["quota_exhausted", { account: "claude:default", window: "five_hour" }],
  [
    "quota_exhausted",
    {
      account: "claude:default",
      window: "five_hour",
      resets_at: "2026-10-31T13:00:00.000Z",
    },
  ],
  [
    "path_scope_violation",
    {
      stage: "dev",
      head_sha: HEAD,
      reason: "outside_scope",
      paths: ["services/web/a.ts"],
      count: 1,
      scope: ["services/api/**"],
    },
  ],
  [
    "path_scope_violation",
    {
      stage: "merge",
      head_sha: HEAD,
      reason: "outside_scope",
      paths: ["services/web/a.ts", "web/b.ts"],
      count: 5,
      scope: ["services/api/**", "docs/**"],
    },
  ],
  [
    "path_scope_violation",
    {
      stage: "dev",
      head_sha: HEAD,
      reason: "too_many_paths",
      paths: [],
      count: 0,
      scope: ["services/api/**"],
    },
  ],
  [
    "path_scope_violation",
    {
      stage: "merge",
      head_sha: HEAD,
      reason: "too_many_paths",
      paths: [],
      count: 0,
      scope: ["services/api/**"],
    },
  ],
  ["no_changes", { dirty: true, commits: 0 }],
  ["no_changes", { dirty: false, commits: 0 }],
  ["push_rejected", { remote_sha: HEAD, local_sha: "f".repeat(40) }],
  ["push_rejected", {}],
  ["scm_unavailable", { phase: "awaiting_ci" }],
  ["scm_unavailable", { phase: "verifying" }],
  ["scm_unavailable", {}],
  ["ci_red", { source: "scm", checks: ["test"], head_sha: HEAD }],
  ["ci_red", { source: "scm", reason: "no_checks", head_sha: HEAD }],
  [
    "ci_red",
    {
      source: "scm",
      reason: "required_missing",
      missing: ["build"],
      head_sha: HEAD,
    },
  ],
  [
    "ci_red",
    {
      source: "saka",
      checks: ["make test"],
      command: "make test",
      exit_code: 2,
      head_sha: HEAD,
    },
  ],
  [
    "ci_red",
    {
      source: "saka",
      checks: ["make test"],
      command: "make test",
      timed_out: true,
      head_sha: HEAD,
    },
  ],
  ["ci_red", { source: "saka", reason: "worktree_moved", head_sha: HEAD }],
  ["ci_red", { source: "saka", reason: "runner_failed" }],
  ["merge_conflict", { reason: "not_mergeable", head_sha: HEAD }],
  ["merge_conflict", { reason: "head_changed", head_sha: HEAD }],
  ["change_closed", { number: 12 }],
  [
    "dod_failed",
    {
      reason: "not_in_base",
      merge_commit_sha: HEAD,
      base_branch: "main",
    },
  ],
  ["dod_failed", {}],
  [
    "dod_failed",
    {
      reason: "verify_failed",
      source: "scm",
      checks: ["test"],
      head_sha: HEAD,
    },
  ],
  [
    "dod_failed",
    {
      reason: "verify_failed",
      source: "saka",
      command: "make test",
      exit_code: 1,
      head_sha: HEAD,
    },
  ],
  [
    "dod_failed",
    {
      reason: "verify_failed",
      source: "saka",
      command: "make test",
      timed_out: true,
      head_sha: HEAD,
    },
  ],
  ["dod_failed", { reason: "no_checks", source: "scm", head_sha: HEAD }],
  [
    "dod_failed",
    {
      reason: "required_missing",
      source: "scm",
      missing: ["build"],
      head_sha: HEAD,
    },
  ],
  ["dod_failed", { reason: "runner_failed", source: "saka", head_sha: HEAD }],
  ["dod_failed", { reason: "worktree_moved", source: "saka", head_sha: HEAD }],
];

const LABELS: Record<string, string[]> = {
  "task.state": [
    "queued",
    "awaiting_start",
    "admitted",
    "in_stage",
    "blocked",
    "taken_over",
    "done",
    "cancelled",
    "superseded",
  ],
  stage: ["dev", "review", "fix", "merge", "sync"],
  phase: [
    "preparing_environment",
    "running_session",
    "pushing",
    "opening_change",
    "awaiting_ci",
    "awaiting_merge",
    "merging",
    "verifying",
    "tearing_down",
  ],
  "run.state": [
    "pending",
    "running",
    "succeeded",
    "failed",
    "blocked",
    "cancelled",
  ],
  "run.trigger": ["auto", "manual"],
  "gate.source": ["scm", "saka"],
  "gate.outcome": ["pending", "passed", "failed"],
  "check.conclusion": [
    "success",
    "failure",
    "neutral",
    "cancelled",
    "skipped",
    "timed_out",
    "action_required",
    "stale",
    "error",
  ],
  "check.status": ["queued", "in_progress", "completed"],
  "change.state": ["open", "merged", "closed"],
  "env.state": ["preparing", "ready", "failed", "teardown_failed", "torn_down"],
  "budget.dimension": ["running_minutes", "turns", "tokens", "cost_usd"],
  "session.state": [
    "starting",
    "running",
    "waiting",
    "idle",
    "interrupted",
    "lost",
    "resumed",
    "ended",
    "failed",
  ],
  "timeline.kind": [
    "system",
    "thought",
    "message",
    "action",
    "output",
    "permission",
    "result",
    "steer",
  ],
  "permission.state": ["pending", "allowed", "denied", "expired"],
  "packet.kind": ["spec", "context"],
  "packet.origin": ["link", "always"],
};

const ERRORS = [
  "network_error",
  "internal",
  "not_ready",
  "not_found",
  "validation_failed",
  "invalid_state",
  "version_stale",
  "precondition_required",
  "merge_not_ready",
  "retry_limit_reached",
  "permission_already_answered",
  "idempotency_key_reused",
  "work_packet_unavailable",
  "project_config_invalid",
  "project_repo_in_use",
  "scm_integration_unknown",
  "scm_repo_inaccessible",
];

describe("catalog coverage", () => {
  for (const locale of ["en", "id"] as const) {
    const t = translator(locale);
    const relative = () => "in 3 hours";

    it(`gives every blocker variant its own sentence in ${locale}`, () => {
      const seen = new Map<string, string>();
      for (const [code, params] of VARIANTS) {
        const sentence = blockerText(t, code, params, relative);
        const where = `${code} ${JSON.stringify(params)}`;
        expect(sentence, where).not.toBe(t("blocker.unknown", { code }));
        expect(sentence, where).not.toMatch(/[{}]/);
        expect(seen.get(sentence), where).toBeUndefined();
        seen.set(sentence, where);
      }
    });

    it(`labels every rendered value in ${locale}`, () => {
      for (const [prefix, values] of Object.entries(LABELS)) {
        for (const value of values) {
          expect(CATALOGS[locale], `${prefix}.${value}`).toHaveProperty([
            `${prefix}.${value}`,
          ]);
        }
      }
      expect(label(t, "run.state", "future_state")).toBe("future_state");
    });

    it(`explains every error code in ${locale}`, () => {
      for (const code of ERRORS) {
        expect(errorText(t, code), code).not.toBe(t("error.unknown", { code }));
        expect(errorText(t, code), code).not.toMatch(/[{}]/);
      }
      expect(errorText(t, "future_error")).toBe(
        t("error.unknown", { code: "future_error" }),
      );
    });
  }

  it("fills the parameters of an error", () => {
    const en = translator("en");
    expect(
      errorText(en, "retry_limit_reached", { stage: "dev", limit: 3 }),
    ).toBe("Stage Development already used its 3 manual retries.");
    expect(
      errorText(en, "merge_not_ready", {
        gate_outcome: "pending",
        head_sha: HEAD,
      }),
    ).toBe(
      "The gate at a1b2c3d4e5f6 is Pending, so the pull request cannot be merged yet.",
    );
  });

  it("names the paths outside the scope and the allowed paths", () => {
    const en = translator("en");
    expect(
      blockerText(en, "path_scope_violation", {
        stage: "merge",
        head_sha: HEAD,
        reason: "outside_scope",
        paths: ["services/web/a.ts", "web/b.ts"],
        count: 5,
        scope: ["services/api/**", "docs/**"],
      }),
    ).toBe(
      "The pull request head a1b2c3d4e5f6 changes 5 files outside the allowed paths: services/web/a.ts, web/b.ts, and 3 more. Allowed paths: services/api/**, docs/**.",
    );
    expect(
      blockerText(en, "path_scope_violation", {
        stage: "dev",
        reason: "outside_scope",
        paths: ["services/web/a.ts"],
        count: 1,
        scope: ["services/api/**"],
      }),
    ).toBe(
      "The commits change 1 file outside the allowed paths: services/web/a.ts. Allowed paths: services/api/**.",
    );
  });

  it("names the evidence of a failed verification after the merge", () => {
    const en = translator("en");
    expect(
      blockerText(en, "dod_failed", {
        reason: "verify_failed",
        source: "saka",
        command: "make test",
        exit_code: 1,
        head_sha: HEAD,
      }),
    ).toBe(
      "After the merge, the Done when command make test failed at a1b2c3d4e5f6 with exit code 1.",
    );
    expect(
      blockerText(en, "dod_failed", {
        reason: "not_in_base",
        merge_commit_sha: HEAD,
        base_branch: "main",
      }),
    ).toBe("The merge is not on main.");
    expect(
      blockerText(
        en,
        "quota_exhausted",
        {
          account: "claude:default",
          window: "five_hour",
          resets_at: "2026-10-31T13:00:00.000Z",
        },
        () => "in 3 hours",
      ),
    ).toBe(
      "The agent account claude:default hit its five_hour rate limit; it resets in 3 hours.",
    );
  });
});
