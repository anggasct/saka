import { readFileSync } from "node:fs";

const locales = ["en", "id"];
const catalogs = Object.fromEntries(
  locales.map((l) => [
    l,
    JSON.parse(
      readFileSync(new URL(`../src/i18n/${l}.json`, import.meta.url), "utf8"),
    ),
  ]),
);
const all = new Set(Object.values(catalogs).flatMap((c) => Object.keys(c)));
let failed = false;
for (const [locale, catalog] of Object.entries(catalogs)) {
  const missing = [...all].filter((key) => !(key in catalog)).sort();
  if (missing.length > 0) {
    failed = true;
    process.stderr.write(`${locale}: missing ${missing.join(", ")}\n`);
  }
}
process.exit(failed ? 1 : 0);
