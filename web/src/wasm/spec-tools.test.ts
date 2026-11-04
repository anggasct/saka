import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";

import { createIntl } from "react-intl";
import { describe, expect, it } from "vitest";

import { CATALOGS, LOCALES, type Locale } from "@/i18n/messages";

import { lintMessage, toDiagnostics, type LinedText } from "./diagnostics";
import {
  loadSpecTools,
  parseLintCodes,
  parseLintReport,
  type LintIssue,
} from "./spec-tools";

const EXAMPLE_PARAMS: Record<string, unknown> = {
  ac: "AC-03",
  after: "S9",
  bytes: 300000,
  column: "Repo",
  command: "make test",
  count: 40,
  cycle: ["a", "b", "a"],
  expected: "S2",
  found: "S3",
  heading: "Storage",
  id: "AC-1",
  item: 2,
  key: "owner",
  limit: 32,
  max: 20,
  max_bytes: 262144,
  max_steps: 50,
  option: 2,
  origin: "link",
  path: "docs/context.md",
  repo: "web",
  reason: "docs",
  slug: "tasks-done",
  step: "S2",
  steps: ["S1", "S2", "S1"],
  target: "../outside.md",
  value: "high",
};

function template(locale: Locale): string {
  return readFileSync(
    resolve(`../crates/saka-spec/templates/${locale}.md`),
    "utf8",
  );
}

function lined(text: string): LinedText {
  const starts = [0];
  for (let i = 0; i < text.length; i++) {
    if (text[i] === "\n") {
      starts.push(i + 1);
    }
  }
  return {
    lines: starts.length,
    line(n) {
      const from = starts[n - 1] ?? 0;
      const next = starts[n];
      return { from, to: next === undefined ? text.length : next - 1 };
    },
  };
}

function translator(locale: Locale) {
  const errors: unknown[] = [];
  const intl = createIntl({
    locale,
    messages: CATALOGS[locale],
    onError: (e) => errors.push(e),
  });
  const t = (key: string, values?: Record<string, string | number>) =>
    intl.formatMessage({ id: key }, values);
  return { t, errors };
}

describe("spec tools", () => {
  it("marks an unknown AC on its step row in both languages", async () => {
    const tools = await loadSpecTools();
    for (const locale of LOCALES) {
      const text = template(locale).replace(
        /^(\| S2 \|[^|]*\| )AC-02( \|)/m,
        "$1AC-09$2",
      );
      expect(text).toContain("AC-09");
      const report = tools.lint("example-spec.md", text);
      expect(report.isSpec).toBe(true);
      const unknown = report.issues.filter((i) => i.code === "step_ac_unknown");
      expect(unknown).toHaveLength(1);
      const s2 = report.steps.find((s) => s.id === "S2");
      expect(unknown[0]?.line).toBe(s2?.line);

      const { t, errors } = translator(locale);
      const diagnostics = toDiagnostics(lined(text), unknown, t);
      const row = text.split("\n")[(s2?.line ?? 0) - 1] ?? "";
      expect(diagnostics).toEqual([
        {
          from: text.indexOf(row),
          to: text.indexOf(row) + row.length,
          severity: "error",
          message: t("lint.step_ac_unknown", { step: "S2", ac: "AC-09" }),
          source: "saka",
        },
      ]);
      expect(diagnostics[0]?.message).toContain("AC-09");
      expect(errors).toEqual([]);
    }
  });

  it("gives a plain document no diagnostics", async () => {
    const tools = await loadSpecTools();
    const report = tools.lint("notes.md", "# Notes\n\nSome text.\n");
    expect(report).toEqual({
      isSpec: false,
      parserVersion: report.parserVersion,
      issues: [],
      steps: [],
    });
    const { t } = translator("en");
    expect(toDiagnostics(lined("# Notes\n"), report.issues, t)).toEqual([]);
  });

  it("has a sentence in every catalog for every lint code", async () => {
    const tools = await loadSpecTools();
    const codes = tools.lintCodes();
    expect(codes.length).toBeGreaterThan(30);
    for (const locale of LOCALES) {
      const { t, errors } = translator(locale);
      const catalog: Record<string, string> = CATALOGS[locale];
      for (const { code, severity } of codes) {
        expect(catalog[`lint.${code}`], `${locale} lint.${code}`).toBeDefined();
        const issue: LintIssue = {
          code,
          severity,
          line: 1,
          params: EXAMPLE_PARAMS,
        };
        const message = lintMessage(issue, t);
        expect(message).not.toBe(t("lint.unknown", { code }));
        expect(message, `${locale} ${code}`).not.toContain("{");
      }
      expect(errors).toEqual([]);
    }
  });

  it("names an unknown code instead of failing", () => {
    const { t } = translator("en");
    const issue: LintIssue = {
      code: "brand_new",
      severity: "warning",
      line: 99,
      params: {},
    };
    const [diagnostic] = toDiagnostics(lined("a\nb"), [issue], t);
    expect(diagnostic?.message).toBe("Lint problem brand_new.");
    expect(diagnostic?.from).toBe(2);
  });

  it("hashes text the way the server hashes the bytes", async () => {
    const tools = await loadSpecTools();
    expect(tools.contentHash("")).toBe(
      "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
    );
    const text = "﻿# Ünï 🚀\r\nline\rlast";
    const expected = createHash("sha256").update(text, "utf8").digest("hex");
    expect(tools.contentHash(text)).toBe(`sha256:${expected}`);
  });

  it("refuses output of the wrong shape", () => {
    expect(() => parseLintReport('{"is_spec": true}')).toThrow();
    expect(() =>
      parseLintReport(
        '{"is_spec":true,"parser_version":4,"issues":[{"code":"x","severity":"fatal","line":1,"params":{}}],"steps":[]}',
      ),
    ).toThrow();
    expect(() => parseLintCodes('[{"code":"x"}]')).toThrow();
  });
});
