import { createIntl } from "react-intl";
import { describe, expect, it } from "vitest";

import { CATALOGS } from "@/i18n/messages";
import type { Locale, MessageKey } from "@/i18n/messages";
import { announce, deploy } from "@/test/hooks-fixtures";

import { hookDetails, hookErrorText, hookSummary } from "./hook-text";

function translator(locale: Locale) {
  const intl = createIntl({ locale, messages: CATALOGS[locale] });
  return (key: MessageKey, values?: Record<string, string | number>) =>
    intl.formatMessage({ id: key }, values);
}

const CODES = [
  "hook_command_failed",
  "hook_timed_out",
  "hook_webhook_rejected",
  "hook_webhook_unavailable",
  "hook_secret_missing",
  "hook_secret_unreadable",
  "hook_environment_failed",
  "hook_commit_unavailable",
  "hook_not_configured",
  "notify_channel_unconfigured",
  "retries_exhausted",
];

describe("hook text", () => {
  for (const locale of ["en", "id"] as const) {
    const t = translator(locale);

    it(`explains every run error in ${locale}`, () => {
      const seen = new Set<string>();
      for (const code of CODES) {
        const sentence = hookErrorText(t, {
          code,
          params: {
            exit_code: 2,
            timeout_secs: 30,
            status: 404,
            name: "KEY",
            channel: "ops",
          },
        });
        expect(sentence, code).not.toMatch(/[{}]/);
        expect(sentence, code).not.toBe(
          hookErrorText(t, { code: "future", params: {} }),
        );
        expect(seen.has(sentence), code).toBe(false);
        seen.add(sentence);
      }
      expect(hookErrorText(t, { code: "future", params: {} })).toContain(
        "future",
      );
    });
  }

  it("fills the parameters of a run error", () => {
    const en = translator("en");
    expect(
      hookErrorText(en, {
        code: "hook_command_failed",
        params: { exit_code: 3 },
      }),
    ).toBe("The command exited with code 3.");
    expect(
      hookErrorText(en, {
        code: "hook_timed_out",
        params: { timeout_secs: 30 },
      }),
    ).toBe("The command ran out of time after 30 s.");
  });

  it("summarises a hook and lists only the details it has", () => {
    const en = translator("en");
    expect(hookSummary(en, deploy)).toBe("on merged, after, run");
    expect(hookDetails(en, deploy).map((row) => row.key)).toEqual([
      "command",
      "secrets",
      "repos",
      "branches",
    ]);
    expect(hookDetails(en, announce).map((row) => row.key)).toEqual([
      "url",
      "secret",
    ]);
  });
});
