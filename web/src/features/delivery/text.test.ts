import { createIntl } from "react-intl";
import { describe, expect, it } from "vitest";

import type { Locale, MessageKey } from "@/i18n/messages";
import { CATALOGS } from "@/i18n/messages";

import { blockerText } from "./text";

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
});
