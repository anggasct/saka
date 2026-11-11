import { createIntl } from "react-intl";
import { describe, expect, it } from "vitest";

import { CATALOGS } from "@/i18n/messages";
import type { Locale, MessageKey } from "@/i18n/messages";

import { diagnosticText, fixText } from "./diagnostic-text";

function translator(locale: Locale) {
  const intl = createIntl({ locale, messages: CATALOGS[locale] });
  return (key: MessageKey, values?: Record<string, string | number>) =>
    intl.formatMessage({ id: key }, values);
}

const relative = (iso: string) => `at ${iso}`;

const FINDINGS: [string, Record<string, unknown>][] = [
  ["token_ok", { name: "saka", scopes: ["api"], expires_at: null }],
  ["repo_access_ok", { access_level: 30 }],
  ["repo_access_ok", { access_level: null }],
  ["repo_webhook_ok", { hook_id: 7, last_received_at: null }],
  ["token_expiring", { name: "saka", expires_at: "2026-12-01T00:00:00.000Z" }],
  ["repo_access_recommended", { access_level: 30, recommended: 40 }],
  ["repo_webhook_unreadable", { status: 403, last_received_at: null }],
  ["repo_webhook_events_missing", { hook_id: 7, missing: ["job_events"] }],
  ["repo_webhook_ssl_unverified", { hook_id: 7 }],
  ["token_rejected", { status: 401 }],
  ["token_inactive", { name: "saka", reason: "revoked" }],
  ["token_inactive", { name: "saka", reason: "expired" }],
  ["token_scopes_missing", { missing: ["api"], granted: ["read_api"] }],
  ["repo_access_missing", { repo: "acme/app", access_level: 20, required: 30 }],
  [
    "repo_access_missing",
    { repo: "acme/app", access_level: null, required: 30 },
  ],
  ["repo_webhook_missing", { path: "/hooks/gitlab" }],
  [
    "repo_webhook_disabled",
    { hook_id: 7, alert_status: "temporarily_disabled", disabled_until: null },
  ],
];

const FIXES: [string, Record<string, unknown>][] = [
  ["create_gitlab_token", { url: "https://gitlab.example/t", scopes: ["api"] }],
  ["grant_repo_access", { url: "https://gitlab.example/m", role: "Developer" }],
  [
    "configure_repo_webhook",
    {
      url: "https://gitlab.example/h",
      path: "/hooks/gitlab",
      events: ["note_events"],
    },
  ],
];

describe("GitLab diagnostic text", () => {
  for (const locale of ["en", "id"] as const) {
    const t = translator(locale);

    it(`words every finding in ${locale} without a raw key or a hole`, () => {
      for (const [code, params] of FINDINGS) {
        const sentence = diagnosticText(t, relative, code, params);
        expect(sentence, code).not.toMatch(/[{}]|diagnostic\./);
        expect(sentence, code).not.toBe(t("diagnostic.unknown", { code }));
      }
    });

    it(`words every fix in ${locale}`, () => {
      for (const [code, params] of FIXES) {
        const sentence = fixText(t, relative, code, params);
        expect(sentence, code).not.toBeNull();
        expect(sentence, code).not.toMatch(/[{}]|diagnosticFix\./);
      }
    });
  }

  it("names the access level of a repo when there is one", () => {
    const t = translator("en");
    expect(
      diagnosticText(t, relative, "repo_access_ok", { access_level: 40 }),
    ).toBe("The token can reach the repo (as Maintainer).");
    expect(
      diagnosticText(t, relative, "repo_access_missing", {
        repo: "acme/app",
        access_level: null,
        required: 30,
      }),
    ).toBe(
      "The token needs access level 30 (Developer) on acme/app; it has no access.",
    );
  });
});
