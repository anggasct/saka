// @vitest-environment node

import { fileURLToPath } from "node:url";

import { ESLint } from "eslint";
import { describe, expect, it } from "vitest";

const root = fileURLToPath(new URL("..", import.meta.url));

async function messagesFor(
  file: string,
): Promise<ESLint.LintResult["messages"]> {
  const eslint = new ESLint({ cwd: root, ignore: false });
  const [result] = await eslint.lintFiles([`eslint-fixtures/${file}`]);
  return result?.messages ?? [];
}

async function rulesFor(file: string): Promise<string[]> {
  return (await messagesFor(file)).map((m) => m.ruleId ?? "fatal");
}

describe("lint fixtures", () => {
  it("refuse every browser global outside the platform layer", async () => {
    const rules = await rulesFor("browser-globals.ts");
    expect(rules).toEqual(Array<string>(10).fill("no-restricted-globals"));
  });

  it("refuse desktop imports outside the platform layer", async () => {
    expect(await rulesFor("tauri-import.ts")).toEqual([
      "no-restricted-imports",
    ]);
  });

  it("refuse doc, block, long, task, commented-out, and suppression comments", async () => {
    const found = (await messagesFor("comments.ts")).map((m) => [
      m.line,
      m.ruleId,
      m.messageId,
    ]);
    expect(found).toEqual([
      [1, "saka/comments", "block"],
      [3, "saka/comments", "block"],
      [4, "saka/comments", "task"],
      [5, "saka/comments", "code"],
      [7, "saka/comments", "long"],
      [11, null, undefined],
    ]);
  });

  it("refuse literal text in JSX", async () => {
    expect(await rulesFor("literal-text.tsx")).toEqual([
      "formatjs/no-literal-string-in-jsx",
    ]);
  });
}, 60_000);
