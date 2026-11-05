import { createIntl } from "react-intl";
import { describe, expect, it } from "vitest";

import { DIAGNOSTIC_CODES } from "@/features/settings/diagnostic-codes";

import { CATALOGS, LOCALES } from "./messages";

const ERROR_CODES = [
  "project_config_invalid",
  "scm_integration_unknown",
  "scm_repo_inaccessible",
  "project_repo_in_use",
  "scm_unavailable",
  "secret_name_invalid",
  "secret_too_short",
  "device_flow_unconfigured",
  "user_authorship_unsupported",
  "user_link_not_found",
  "user_link_in_use",
  "user_link_missing",
  "user_token_missing",
  "user_token_invalid",
  "user_token_in_use",
  "user_token_multi_integration",
  "user_token_not_fine_grained",
  "user_token_rejected",
  "user_token_repo_inaccessible",
];
const CONFIG_ERRORS = [
  "project_config_invalid",
  "scm_integration_unknown",
  "scm_repo_inaccessible",
  "project_repo_in_use",
  "unknown",
];
const CONFIG_ISSUES = [
  "file_missing",
  "toml_invalid",
  "field_missing",
  "field_invalid",
  "field_unknown",
  "too_many_repos",
  "repo_name_duplicate",
  "repo_url_duplicate",
  "default_missing",
  "default_duplicate",
  "path_scope_invalid",
  "path_scope_empty",
  "base_branch_not_found",
  "unknown",
];
const FAILURES = [
  "device_flow_disabled",
  "incorrect_client_credentials",
  "incorrect_device_code",
  "unsupported_grant_type",
  "bad_verification_code",
  "token_not_expiring",
  "identity_unavailable",
];

const diagnostics = [
  ...DIAGNOSTIC_CODES.pass,
  ...DIAGNOSTIC_CODES.warn,
  ...DIAGNOSTIC_CODES.fail,
  ...DIAGNOSTIC_CODES.skip,
].map((code) => `diagnostic.${code}`);

const REQUIRED = [
  ...diagnostics,
  "diagnostic.unknown",
  ...DIAGNOSTIC_CODES.fix.map((code) => `diagnosticFix.${code}`),
  ...ERROR_CODES.map((code) => `error.${code}`),
  ...CONFIG_ERRORS.map((code) => `configError.${code}`),
  ...CONFIG_ISSUES.map((code) => `projectConfigIssue.${code}`),
  "settings.link.waiting",
  "settings.link.linked",
  "settings.link.expired",
  "settings.link.denied",
  "settings.link.failed",
  ...FAILURES.map((code) => `settings.link.failure.${code}`),
  ...Object.keys(CATALOGS.en).filter((key) => key.startsWith("settings.")),
];

function filled(message: string): Record<string, string | number> {
  const values: Record<string, string | number> = {};
  for (const match of message.matchAll(/\{(\w+)/g)) {
    values[match[1] ?? ""] = "x";
  }
  return values;
}

describe("settings catalogs", () => {
  it("name every required code in both locales", () => {
    for (const locale of LOCALES) {
      const catalog: Record<string, string> = CATALOGS[locale];
      const missing = REQUIRED.filter((key) => !(key in catalog));
      expect(missing, locale).toEqual([]);
    }
  });

  it("hold real sentences without raw ICU syntax", () => {
    for (const locale of LOCALES) {
      const errors: unknown[] = [];
      const catalog: Record<string, string> = CATALOGS[locale];
      const intl = createIntl({
        locale,
        messages: catalog,
        onError: (e) => errors.push(e),
      });
      for (const key of REQUIRED) {
        const message = catalog[key] ?? "";
        expect(message, `${locale} ${key}`).not.toBe("");
        const rendered = intl.formatMessage(
          { id: key },
          filled((CATALOGS.en as Record<string, string>)[key] ?? message),
        );
        expect(rendered, `${locale} ${key}`).not.toMatch(/[{}]|\bselect,/);
      }
      expect(errors).toEqual([]);
    }
  });

  it("translate Indonesian apart from English for diagnosis sentences", () => {
    const same = [...diagnostics].filter(
      (key) =>
        (CATALOGS.id as Record<string, string>)[key] ===
        (CATALOGS.en as Record<string, string>)[key],
    );
    expect(same).toEqual([]);
  });
});
