// Drives the prototype in headless Chromium: the whole-vault round trip,
// or keystroke latency on one note.
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
const { chromium } = require("playwright");

const [mode, arg, lineArg] = process.argv.slice(2);
const browser = await chromium.launch({ headless: true });
const page = await browser.newPage({ viewport: { width: 1280, height: 900 } });
page.on("console", (m) => m.type() === "error" && console.log("console:", m.text()));
page.on("pageerror", (e) => console.log("pageerror:", e.message));
await page.goto(process.env.BASE ?? "http://127.0.0.1:7420/");
await page.evaluate(() => window.saka.ready);

if (mode === "roundtrip") {
  const r = await page.evaluate(() => window.saka.roundTrip({ concurrency: 6 }));
  console.log(JSON.stringify(r, null, 1));
} else if (mode === "perf") {
  const t0 = Date.now();
  await page.evaluate((p) => window.saka.openFile(p), arg);
  await page.waitForSelector(".cm-content");
  const openMs = Date.now() - t0;
  const info = await page.evaluate((n) => window.saka.perfStart(n), lineArg ? Number(lineArg) : undefined);
  await page.evaluate(() => {
    window.__long = [];
    new PerformanceObserver((l) => l.getEntries().forEach((e) => window.__long.push(e.duration))).observe({ type: "longtask" });
  });
  await page.keyboard.type("The quick brown fox jumps over the lazy dog. ".repeat(7), { delay: 25 });
  await page.keyboard.press("Enter", { delay: 25 });
  await page.keyboard.type("- [ ] a new task with a [[wiki link]] in it", { delay: 25 });
  for (let i = 0; i < 40; i++) await page.keyboard.press("Backspace", { delay: 25 });
  await page.waitForTimeout(500);
  const r = await page.evaluate(() => ({ ...window.saka.perfResult(), longTasks: window.__long.length, longestTaskMs: Math.max(0, ...window.__long) }));
  console.log(JSON.stringify({ path: arg, openMs, ...info, ...r }));
}
await browser.close();
