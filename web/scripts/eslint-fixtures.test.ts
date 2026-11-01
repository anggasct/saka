// @vitest-environment node
// The lint rules that keep feature code off browser and desktop APIs and off literal
// text are proven by files that break them.

import { fileURLToPath } from "node:url";

import { ESLint } from "eslint";
import { describe, expect, it } from "vitest";

const root = fileURLToPath(new URL("..", import.meta.url));

async function rulesFor(file: string): Promise<string[]> {
  const eslint = new ESLint({ cwd: root, ignore: false });
  const [result] = await eslint.lintFiles([`eslint-fixtures/${file}`]);
  return (result?.messages ?? []).map((m) => m.ruleId ?? "fatal");
}

describe("lint fixtures", () => {
  it("refuse every browser global outside the platform layer", async () => {
    const rules = await rulesFor("browser-globals.ts");
    expect(rules).toEqual(Array<string>(7).fill("no-restricted-globals"));
  });

  it("refuse desktop imports outside the platform layer", async () => {
    expect(await rulesFor("tauri-import.ts")).toEqual([
      "no-restricted-imports",
    ]);
  });

  it("refuse literal text in JSX", async () => {
    expect(await rulesFor("literal-text.tsx")).toEqual([
      "formatjs/no-literal-string-in-jsx",
    ]);
  });
}, 60_000);
