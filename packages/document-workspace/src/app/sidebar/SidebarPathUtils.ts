// Path/label helper functions used by the sidebar's document context menu (Copy Path / Copy Relative
// Path / Copy File Name / Copy Folder Path / Copy Breadcrumbs Path) and by the "local file" predicate
// used to decide whether pin/reveal-in-explorer actions apply to a document.
// Extracted verbatim from Sidebar.tsx as part of the 2026-07-02 decomposition plan (V-K).
import type { LoadedDocument } from "@rightkit/document-core/documents/types";
import { toast } from "@rightkit/platform-ui/headless";
import { writeClipboardText } from "@rightkit/document-core/state/tauriBridge";

export function isLocalFileDoc(doc: LoadedDocument): boolean {
  return doc.format !== "web" && !doc.path.startsWith("untitled:");
}

export function pathFolder(path: string): string {
  const lastSlash = Math.max(path.lastIndexOf("\\"), path.lastIndexOf("/"));
  return lastSlash <= 0 ? "" : path.slice(0, lastSlash);
}

export function pathBase(path: string): string {
  const lastSlash = Math.max(path.lastIndexOf("\\"), path.lastIndexOf("/"));
  return lastSlash < 0 ? path : path.slice(lastSlash + 1);
}

export function pathSeparator(path: string): "\\" | "/" {
  return path.includes("\\") ? "\\" : "/";
}

export function splitPath(path: string): string[] {
  return path.split(/[\\/]+/).filter(Boolean);
}

export function commonFolder(paths: string[]): string {
  if (paths.length < 2) return pathFolder(paths[0] ?? "");
  const sep = pathSeparator(paths[0]);
  const dirs = paths.map((path) => splitPath(pathFolder(path)));
  const first = dirs[0] ?? [];
  const common: string[] = [];
  for (let i = 0; i < first.length; i++) {
    const part = first[i];
    if (dirs.every((dir) => dir[i]?.toLowerCase() === part.toLowerCase())) common.push(part);
    else break;
  }
  return common.join(sep);
}

export function relativePath(path: string, openLocalPaths: string[]): string {
  const base = commonFolder(openLocalPaths);
  if (!base) return pathBase(path);
  const next = path.slice(base.length).replace(/^[\\/]+/, "");
  return next || pathBase(path);
}

export function breadcrumbPath(path: string): string {
  return splitPath(path).join(" > ");
}

export async function copyText(text: string) {
  try {
    await writeClipboardText(text);
    toast("Copied to clipboard", "success");
  } catch {
    // Clipboard can reject (permissions / no focus) — don't leak an unhandled rejection.
    toast("Couldn't copy to clipboard", "error");
  }
}
