import { defaultKeymap, history, historyKeymap } from "@codemirror/commands";
import { markdown, markdownLanguage } from "@codemirror/lang-markdown";
import { defaultHighlightStyle, ensureSyntaxTree, syntaxHighlighting, syntaxTree } from "@codemirror/language";
import { EditorState, type Extension } from "@codemirror/state";
import { drawSelection, EditorView, highlightSpecialChars, keymap } from "@codemirror/view";
import { livePreview } from "./livepreview";
import { Resolver } from "./resolve";
import { Frontmatter, WikiLink } from "./syntax";
import "./style.css";

interface FileData {
  path: string;
  binary: boolean;
  text: string;
  bom: boolean;
  eol: "lf" | "crlf" | "mixed" | "none";
  bytes: number;
}

interface LinkRow {
  source: string;
  line: number;
  target: string;
  embed: boolean;
  dest: string | null;
}

let files: string[] = [];
let resolver = new Resolver([]);
let view: EditorView | null = null;
let current: FileData | null = null;
let saveTimer = 0;

const enc = encodeURIComponent;

/** fetch with one retry: Chromium aborts in-flight requests when the host's network interfaces change. */
async function fetchRetry(url: string, init?: RequestInit): Promise<Response> {
  try {
    return await fetch(url, init);
  } catch {
    await new Promise((r) => setTimeout(r, 500));
    return fetch(url, init);
  }
}

async function getJson<T>(url: string): Promise<T> {
  const r = await fetchRetry(url);
  if (!r.ok) throw new Error(`${url}: ${r.status}`);
  return (await r.json()) as T;
}

async function put(path: string, text: string, bom: boolean, force: boolean): Promise<boolean> {
  const r = await fetchRetry(`/api/file?path=${enc(path)}${force ? "&force=true" : ""}`, {
    method: "PUT",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ text, bom }),
  });
  return r.ok;
}

function extensions(f: FileData, extra: Extension[] = []): Extension[] {
  return [
    // Lossless text: a CRLF file splits on CRLF; any other file splits on LF
    // only, so a stray CR stays part of its line and survives the save.
    // Serialize with state.sliceDoc(): doc.toString() always joins with LF.
    EditorState.lineSeparator.of(f.eol === "crlf" ? "\r\n" : "\n"),
    markdown({ base: markdownLanguage, extensions: [WikiLink, Frontmatter] }),
    syntaxHighlighting(defaultHighlightStyle, { fallback: true }),
    history(),
    drawSelection(),
    highlightSpecialChars(),
    keymap.of([...defaultKeymap, ...historyKeymap]),
    EditorView.lineWrapping,
    livePreview({ path: f.path, resolver, open: (p) => void openFile(p) }),
    ...extra,
  ];
}

async function openFile(path: string): Promise<void> {
  const f = await getJson<FileData>(`/api/file?path=${enc(path)}`);
  const host = document.getElementById("editor")!;
  view?.destroy();
  current = f;
  document.getElementById("title")!.textContent = f.binary ? `${path} (binary, read-only)` : path;
  if (f.binary) {
    view = null;
    return;
  }
  const autosave = EditorView.updateListener.of((u) => {
    if (!u.docChanged) return;
    window.clearTimeout(saveTimer);
    saveTimer = window.setTimeout(() => {
      if (view && current) void put(current.path, view.state.sliceDoc(), current.bom, false);
    }, 800);
  });
  view = new EditorView({ state: EditorState.create({ doc: f.text, extensions: extensions(f, [autosave]) }), parent: host });
  window.history.replaceState(null, "", `#${enc(path)}`);
}

function linksIn(state: EditorState, source: string, out: LinkRow[]): void {
  syntaxTree(state).iterate({
    enter: (n) => {
      if (n.name !== "WikiLink" && n.name !== "Embed") return;
      const t = n.node.getChild("WikiLinkTarget");
      const target = t ? state.doc.sliceString(t.from, t.to) : "";
      out.push({
        source,
        line: state.doc.lineAt(n.from).number,
        target,
        embed: n.name === "Embed",
        dest: resolver.resolve(target, source),
      });
      return false;
    },
  });
}

/** Ticking the first task must change exactly one character. */
function taskEditIsLocal(state: EditorState, original: string): boolean | null {
  let pos = -1;
  syntaxTree(state).iterate({
    enter: (n) => {
      if (pos < 0 && n.name === "TaskMarker") pos = n.from;
    },
  });
  if (pos < 0) return null;
  const was = state.sliceDoc(pos + 1, pos + 2);
  const next = state.update({ changes: { from: pos + 1, to: pos + 2, insert: was === " " ? "x" : " " } }).state.sliceDoc();
  if (next.length !== original.length) return false;
  let diffs = 0;
  for (let i = 0; i < next.length && diffs < 2; i++) if (next[i] !== original[i]) diffs++;
  return diffs === 1;
}

/** Editing one property value must change exactly one line. */
function propertyEditIsLocal(state: EditorState, original: string, sep: string): boolean | null {
  const first = syntaxTree(state).topNode.firstChild;
  if (!first || first.name !== "Frontmatter") return null;
  for (let n = state.doc.lineAt(first.from).number + 1; n < state.doc.lineAt(first.to).number; n++) {
    const line = state.doc.line(n);
    const m = /^([^\s#:][^:]*):[ \t]*(\S.*?)\r?$/.exec(line.text);
    if (!m) continue;
    const from = line.from + line.text.length - m[2].length - (line.text.endsWith("\r") ? 1 : 0);
    const next = state.update({ changes: { from, to: from + m[2].length, insert: "edited" } }).state.sliceDoc();
    const a = original.split(sep);
    const b = next.split(sep);
    return a.length === b.length && a.filter((l, i) => l !== b[i]).length === 1;
  }
  return null;
}

async function roundTrip(opts: { concurrency?: number; limit?: number } = {}) {
  const md = files.filter((f) => f.endsWith(".md")).slice(0, opts.limit ?? Infinity);
  const host = document.createElement("div");
  host.style.cssText = "position:absolute;left:-10000px;top:0;width:900px;height:700px;overflow:auto";
  document.body.append(host);
  const res = {
    total: md.length,
    written: 0,
    binary: [] as string[],
    changedInBrowser: [] as string[],
    taskEdits: { local: 0, broken: [] as string[] },
    propertyEdits: { local: 0, broken: [] as string[] },
    links: [] as LinkRow[],
    errors: [] as string[],
    ms: 0,
  };
  const t0 = performance.now();
  let next = 0;
  const worker = async () => {
    while (next < md.length) {
      const path = md[next++];
      try {
        const f = await getJson<FileData>(`/api/file?path=${enc(path)}`);
        if (f.binary) {
          res.binary.push(path);
          continue;
        }
        const v = new EditorView({ state: EditorState.create({ doc: f.text, extensions: extensions(f) }), parent: host });
        ensureSyntaxTree(v.state, v.state.doc.length, 20000);
        v.dispatch({ selection: { anchor: Math.min(1, v.state.doc.length) } });
        const out = v.state.sliceDoc();
        if (out !== f.text) res.changedInBrowser.push(path);
        linksIn(v.state, path, res.links);
        const sep = f.eol === "crlf" ? "\r\n" : "\n";
        const task = taskEditIsLocal(v.state, f.text);
        if (task === true) res.taskEdits.local++;
        else if (task === false) res.taskEdits.broken.push(path);
        const prop = propertyEditIsLocal(v.state, f.text, sep);
        if (prop === true) res.propertyEdits.local++;
        else if (prop === false) res.propertyEdits.broken.push(path);
        v.destroy();
        if (await put(path, out, f.bom, true)) res.written++;
        else res.errors.push(`${path}: save failed`);
      } catch (e) {
        res.errors.push(`${path}: ${e}`);
      }
    }
  };
  await Promise.all(Array.from({ length: opts.concurrency ?? 6 }, worker));
  res.ms = performance.now() - t0;
  host.remove();
  await fetch("/api/report", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ name: "roundtrip", data: res }),
  });
  return {
    total: res.total,
    written: res.written,
    binary: res.binary.length,
    changedInBrowser: res.changedInBrowser,
    taskEdits: { local: res.taskEdits.local, broken: res.taskEdits.broken.length },
    propertyEdits: { local: res.propertyEdits.local, broken: res.propertyEdits.broken.length },
    links: res.links.length,
    unresolved: res.links.filter((l) => !l.dest).length,
    errors: res.errors.slice(0, 5),
    seconds: Math.round(res.ms / 100) / 10,
  };
}

/** Keystroke latency: from keydown to the frame after the editor updated. */
const latencies: number[] = [];
let measuring = false;
function perfStart(lineNo?: number): { length: number; lines: number; lineLength: number } {
  if (!view) throw new Error("no file open");
  const v = view;
  if (!measuring) {
    measuring = true;
    v.contentDOM.addEventListener(
      "keydown",
      () => {
        const t0 = performance.now();
        requestAnimationFrame(() => setTimeout(() => latencies.push(performance.now() - t0), 0));
      },
      true,
    );
  }
  latencies.length = 0;
  const at = lineNo ? v.state.doc.line(lineNo) : v.state.doc.lineAt(Math.floor(v.state.doc.length / 2));
  v.dispatch({ selection: { anchor: lineNo ? at.from + Math.floor(at.length / 2) : at.to }, scrollIntoView: true });
  v.focus();
  return { length: v.state.doc.length, lines: v.state.doc.lines, lineLength: at.length };
}

function perfResult() {
  const s = [...latencies].sort((a, b) => a - b);
  const q = (p: number) => (s.length ? Math.round(s[Math.min(s.length - 1, Math.floor(p * s.length))] * 10) / 10 : null);
  return { keys: s.length, p50: q(0.5), p95: q(0.95), p99: q(0.99), max: q(1) };
}

async function boot(): Promise<void> {
  files = (await getJson<{ files: string[] }>("/api/files")).files;
  resolver = new Resolver(files);
  const list = document.getElementById("files")!;
  const filter = document.getElementById("filter") as HTMLInputElement;
  const render = () => {
    const q = filter.value.toLowerCase();
    list.replaceChildren(
      ...files
        .filter((f) => f.endsWith(".md") && f.toLowerCase().includes(q))
        .slice(0, 400)
        .map((f) => {
          const a = document.createElement("a");
          a.textContent = f;
          a.href = `#${enc(f)}`;
          a.onclick = (e) => {
            e.preventDefault();
            void openFile(f);
          };
          return a;
        }),
    );
  };
  filter.oninput = render;
  render();
  const start = decodeURIComponent(location.hash.slice(1));
  if (start) await openFile(start);
}

declare global {
  interface Window {
    saka: Record<string, unknown>;
  }
}
window.saka = { roundTrip, openFile, perfStart, perfResult, ready: boot() };
