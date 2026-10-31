// Replays Obsidian's own link list through our resolver.
import { readFileSync } from "node:fs";
import { Resolver } from "../src/resolve.ts";

interface Row { source: string; line: number; target: string; embed: boolean; dest: string | null }
const files: string[] = JSON.parse(readFileSync(process.argv[2], "utf8")).files;
const rows: Row[] = JSON.parse(readFileSync(process.argv[3], "utf8")).rows.filter((r: Row) => !r.source.startsWith("1-projects/aura/aura/"));
const resolver = new Resolver(files);
const diffs = rows.filter((r) => resolver.resolve(r.target, r.source) !== r.dest);
console.log(`same as Obsidian: ${rows.length - diffs.length}/${rows.length} = ${(100 * (rows.length - diffs.length) / rows.length).toFixed(3)}%`);
for (const d of diffs.slice(0, 8)) console.log(" ", d.source, `L${d.line}`, JSON.stringify(d.target), "obsidian:", d.dest, "ours:", resolver.resolve(d.target, d.source));
