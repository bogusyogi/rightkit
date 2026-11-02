import type { MarkdownIndexedFile } from "./references/folderIndex.js";

export interface WikiLinkResolutionOptions {
  documentPath?: string;
  folderFiles?: readonly MarkdownIndexedFile[];
  sessionFiles?: readonly MarkdownIndexedFile[];
}

/** Default label to show when a wiki-link omits an explicit display. */
export function defaultWikiLinkDisplay(target: string, hash: string): string {
  if (!target) return hash ? `#${hash}` : "";
  const base = target.split("/").pop() ?? target;
  if (!hash) return base;
  return `${base}#${hash}`;
}

/**
 * Resolve a wiki-link target using the folder/session indices.
 *
 * Rules:
 *  - exact relative-path match wins;
 *  - otherwise unique basename match wins;
 *  - ambiguous or missing matches return `null` (no auto-choice).
 */
export function resolveWikiLink(
  target: string,
  options: WikiLinkResolutionOptions,
): MarkdownIndexedFile | null {
  if (!target) return null;
  const needle = normalizeWikiTarget(target);
  const all = [...(options.folderFiles ?? []), ...(options.sessionFiles ?? [])];

  const exact = all.find((f) => normalizeWikiTarget(f.relativePath) === needle);
  if (exact) return exact;

  const base = needle.split("/").pop() ?? needle;
  const byBase = all.filter((f) => (f.relativePath.split("/").pop() ?? f.relativePath) === base);
  if (byBase.length === 1) return byBase[0] ?? null;

  const lowerBase = base.toLowerCase();
  const byBaseCI = all.filter(
    (f) => (f.relativePath.split("/").pop() ?? f.relativePath).toLowerCase() === lowerBase,
  );
  if (byBaseCI.length === 1) return byBaseCI[0] ?? null;

  return null;
}

function normalizeWikiTarget(target: string): string {
  let t = target.trim();
  if (t.startsWith("./")) t = t.slice(2);
  return t;
}
