// Asks a running Obsidian (via its DevTools port) how it resolves every
// wiki-link in the open vault: the oracle for the editor's resolver.
import { writeFileSync } from "node:fs";
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
const { chromium } = require("playwright");

const [endpoint, out] = process.argv.slice(2);
const browser = await chromium.connectOverCDP(endpoint);
let page;
for (let i = 0; i < 60 && !page; i++) {
  page = browser.contexts().flatMap((c) => c.pages()).find((p) => p.url().startsWith("app://"));
  if (!page) await new Promise((r) => setTimeout(r, 1000));
}
if (!page) throw new Error("no Obsidian window");
await page.waitForFunction(() => typeof window.app?.metadataCache?.getFileCache === "function", null, { timeout: 120000 });
const result = await page.evaluate(async () => {
  const mc = window.app.metadataCache;
  const files = window.app.vault.getMarkdownFiles();
  let stable = 0;
  let last = -1;
  for (let i = 0; i < 900 && stable < 5; i++) {
    const cached = files.filter((f) => mc.getFileCache(f)).length;
    const resolved = Object.keys(mc.resolvedLinks).length;
    stable = cached === files.length && resolved === last ? stable + 1 : 0;
    last = resolved;
    await new Promise((r) => setTimeout(r, 1000));
  }
  const rows = [];
  for (const f of files) {
    const c = mc.getFileCache(f);
    if (!c) continue;
    for (const [embed, list] of [[false, c.links ?? []], [true, c.embeds ?? []]]) {
      for (const l of list) {
        if (!l.original.startsWith(embed ? "![[" : "[[")) continue;
        const dest = mc.getFirstLinkpathDest(l.link.replace(/#.*$/, ""), f.path);
        rows.push({ source: f.path, line: l.position.start.line + 1, target: l.link, embed, dest: dest ? dest.path : null });
      }
    }
  }
  return { version: window.electron?.ipcRenderer ? "desktop" : "?", files: files.length, rows };
});
writeFileSync(out, JSON.stringify(result));
console.log(`obsidian: ${result.files} notes, ${result.rows.length} wiki-links -> ${out}`);
await browser.close();
