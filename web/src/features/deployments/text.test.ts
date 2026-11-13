import { createIntl } from "react-intl";
import { describe, expect, it } from "vitest";

import type { Locale, MessageKey } from "@/i18n/messages";
import { CATALOGS } from "@/i18n/messages";

import { deployErrorText, gateReason, isCancellable } from "./text";
import { deployment } from "@/test/deploy-fixtures";

function translator(locale: Locale) {
  const intl = createIntl({ locale, messages: CATALOGS[locale] });
  return (key: MessageKey, values?: Record<string, string | number>) =>
    intl.formatMessage({ id: key }, values);
}

const ERRORS: [string, Record<string, unknown>][] = [
  ["deploy_branch_unavailable", {}],
  ["deploy_command_failed", { exit_code: 2 }],
  ["deploy_timed_out", {}],
  ["deploy_webhook_rejected", { status: 502 }],
  ["deploy_webhook_unavailable", {}],
  ["deploy_interrupted", {}],
  ["deploy_environment_failed", {}],
  ["deploy_secret_missing", { name: "DEPLOY_TOKEN" }],
  ["ready_timed_out", { attempts: 5 }],
  ["verify_failed", { failed: 1, total: 3 }],
  ["verify_environment_failed", {}],
];

describe("deployErrorText", () => {
  for (const locale of ["en", "id"] as const) {
    const t = translator(locale);

    it(`gives every deployment error its own sentence in ${locale}`, () => {
      const seen = new Set<string>();
      for (const [code, params] of ERRORS) {
        const sentence = deployErrorText(t, { code, params });
        expect(sentence, code).not.toMatch(/[{}]/);
        expect(sentence, code).not.toBe(
          deployErrorText(t, { code: "future_error", params: {} }),
        );
        expect(seen.has(sentence), code).toBe(false);
        seen.add(sentence);
      }
    });

    it(`still reads well without the parameters in ${locale}`, () => {
      for (const [code] of ERRORS) {
        const sentence = deployErrorText(t, { code, params: {} });
        expect(sentence, code).not.toMatch(/[{}]/);
        expect(sentence, code).not.toMatch(/undefined|NaN/);
      }
    });

    it(`names an unknown code in ${locale}`, () => {
      expect(deployErrorText(t, { code: "future_error" })).toBe(
        t("deploy.error.unknown", { code: "future_error" }),
      );
    });
  }

  it("fills the parameters in", () => {
    const t = translator("en");
    expect(
      deployErrorText(t, {
        code: "deploy_command_failed",
        params: { exit_code: 2 },
      }),
    ).toBe("The deploy command exited with code 2.");
    expect(
      deployErrorText(t, {
        code: "verify_failed",
        params: { failed: 1, total: 3 },
      }),
    ).toBe("1 of 3 verification commands failed.");
    expect(
      deployErrorText(t, { code: "ready_timed_out", params: { attempts: 1 } }),
    ).toBe("The readiness check did not pass after 1 attempt.");
    expect(
      deployErrorText(translator("id"), {
        code: "deploy_secret_missing",
        params: { name: "TOKEN" },
      }),
    ).toBe("Secret TOKEN yang dibutuhkan deploy belum diatur.");
  });

  it("only lets a deployment that waits or verifies be cancelled", () => {
    expect(isCancellable(deployment({ state: "queued" }))).toBe(true);
    expect(isCancellable(deployment({ state: "awaiting_ready" }))).toBe(true);
    expect(isCancellable(deployment({ state: "verifying" }))).toBe(true);
    expect(isCancellable(deployment({ state: "deploying" }))).toBe(false);
    expect(isCancellable(deployment({ state: "failed" }))).toBe(false);
  });

  it("explains a skipped gate and keeps the operator's own words", () => {
    const t = translator("en");
    expect(gateReason(t, "skipped", "environment_removed")).toBe(
      "The environment was removed.",
    );
    expect(gateReason(t, "accepted", "staging was down")).toBe(
      "staging was down",
    );
  });
});
