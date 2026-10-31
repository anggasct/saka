// Wiki-link resolution modelled on Obsidian: case-insensitive, `.md`
// implied, a bare name matches by file name anywhere in the vault.

export class Resolver {
  private byPath = new Map<string, string>();
  private byName = new Map<string, string[]>();

  constructor(files: string[]) {
    for (const f of files) {
      const key = stripMd(f).toLowerCase();
      this.byPath.set(key, f);
      const name = key.slice(key.lastIndexOf("/") + 1);
      const list = this.byName.get(name);
      if (list) list.push(f);
      else this.byName.set(name, [f]);
    }
  }

  /** The file a link points at, or null when it is unresolved. */
  resolve(target: string, source: string): string | null {
    const link = target.split(/[#^]/)[0].trim();
    if (link === "") return source;
    const dir = source.includes("/") ? source.slice(0, source.lastIndexOf("/")) : "";
    if (link.startsWith("./") || link.startsWith("../")) {
      return this.byPath.get(stripMd(normalize(`${dir}/${link}`)).toLowerCase()) ?? null;
    }
    const want = stripMd(link.replace(/^\/+/, "")).toLowerCase();
    const exact = this.byPath.get(want);
    if (exact) return exact;
    const rel = this.byPath.get(dir ? `${dir.toLowerCase()}/${want}` : want);
    if (rel) return rel;
    const name = want.slice(want.lastIndexOf("/") + 1);
    const hits = (this.byName.get(name) ?? []).filter((f) => {
      const k = stripMd(f).toLowerCase();
      return k === want || k.endsWith(`/${want}`);
    });
    if (hits.length === 0) return null;
    // Matches Obsidian on a real vault: files under the linking note's
    // folder win, then the shortest path.
    hits.sort((a, b) => under(b, dir) - under(a, dir) || a.length - b.length || a.localeCompare(b));
    return hits[0];
  }
}

function under(f: string, dir: string): number {
  return dir === "" || f.toLowerCase().startsWith(`${dir.toLowerCase()}/`) ? 1 : 0;
}

function stripMd(p: string): string {
  return p.endsWith(".md") ? p.slice(0, -3) : p;
}

function normalize(p: string): string {
  const out: string[] = [];
  for (const part of p.split("/")) {
    if (part === "" || part === ".") continue;
    if (part === "..") out.pop();
    else out.push(part);
  }
  return out.join("/");
}
