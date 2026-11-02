import { Link } from "@tanstack/react-router";
import { useState } from "react";

import { useT } from "@/i18n/use-t";

import type { FileTree as Tree } from "./queries";

type Folder = {
  name: string;
  key: string;
  folders: Map<string, Folder>;
  files: { name: string; path: string }[];
};

function relative(projectPath: string, path: string): string {
  return projectPath === "." ? path : path.slice(projectPath.length + 1);
}

export function buildTree(tree: Tree): Folder {
  const root: Folder = { name: "", key: "", folders: new Map(), files: [] };
  for (const file of tree.files) {
    const parts = relative(tree.project_path, file.path).split("/");
    const name = parts.pop() ?? file.path;
    let folder = root;
    for (const part of parts) {
      let next = folder.folders.get(part);
      if (next === undefined) {
        next = {
          name: part,
          key: folder.key === "" ? part : `${folder.key}/${part}`,
          folders: new Map(),
          files: [],
        };
        folder.folders.set(part, next);
      }
      folder = next;
    }
    folder.files.push({ name, path: file.path });
  }
  return root;
}

function ancestorsOf(projectPath: string, path: string | undefined): string[] {
  if (path === undefined) {
    return [];
  }
  const parts = relative(projectPath, path).split("/").slice(0, -1);
  return parts.map((_, i) => parts.slice(0, i + 1).join("/"));
}

export function FileTree({
  projectId,
  tree,
  openPath,
}: {
  projectId: string;
  tree: Tree;
  openPath: string | undefined;
}) {
  const t = useT();
  const [open, setOpen] = useState(
    () => new Set(ancestorsOf(tree.project_path, openPath)),
  );
  const toggle = (key: string) => {
    setOpen((current) => {
      const next = new Set(current);
      if (next.has(key)) {
        next.delete(key);
      } else {
        next.add(key);
      }
      return next;
    });
  };
  const root = buildTree(tree);

  const renderFolder = (folder: Folder) => (
    <ul className="space-y-0.5">
      {[...folder.folders.values()].map((child) => {
        const expanded = open.has(child.key);
        return (
          <li key={`d:${child.key}`}>
            <button
              type="button"
              aria-expanded={expanded}
              className="flex min-h-11 w-full items-center gap-1 rounded px-1 text-left text-sm font-medium hover:bg-muted lg:min-h-8"
              onClick={() => {
                toggle(child.key);
              }}
            >
              <svg
                aria-hidden="true"
                viewBox="0 0 16 16"
                className={`size-4 shrink-0 transition-transform ${expanded ? "rotate-90" : ""}`}
              >
                <path
                  d="M6 4l4 4-4 4"
                  fill="none"
                  stroke="currentColor"
                  strokeWidth="1.5"
                />
              </svg>
              {child.name}
            </button>
            {expanded && <div className="pl-4">{renderFolder(child)}</div>}
          </li>
        );
      })}
      {folder.files.map((file) => (
        <li key={`f:${file.path}`}>
          <Link
            to="/docs/$projectId"
            params={{ projectId }}
            search={{ path: file.path }}
            aria-current={file.path === openPath ? "page" : undefined}
            className="flex min-h-11 items-center truncate rounded px-1 text-sm hover:bg-muted aria-[current=page]:bg-muted aria-[current=page]:font-semibold lg:min-h-8"
          >
            {file.name}
          </Link>
        </li>
      ))}
    </ul>
  );

  return (
    <div className="space-y-2">
      {tree.files.length === 0 ? (
        <p className="text-sm text-muted-foreground">
          {t("docs.tree.empty", { path: tree.project_path })}
        </p>
      ) : (
        renderFolder(root)
      )}
      {tree.truncated && (
        <p className="text-xs text-muted-foreground">
          {t("docs.tree.truncated", { count: tree.files.length })}
        </p>
      )}
    </div>
  );
}
