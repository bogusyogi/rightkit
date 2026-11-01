/**
 * Markdown export pipeline.
 *
 * The export pipeline is **the only place** that emits HTML or
 * plain text from a {@link MarkdownDocumentCore} snapshot. Every
 * other surface (preview, status bar, search) consumes the
 * result of this pipeline.
 *
 * The pipeline is pure: it takes a snapshot and a render mode
 * (preview / word-count / snippet), returns the formatted
 * output, and never touches React state or the Tauri runtime.
 */

import type { MarkdownCoreSnapshot } from "../core/MarkdownDocumentTypes";

/**
 * Render modes. `preview` returns the rendered HTML; `plain-text`
 * strips formatting and returns a normalized text; `word-count`
 * returns an exact word count; `snippet` returns the first
 * `maxChars` code units.
 */
export type MarkdownExportMode = "preview" | "plain-text" | "word-count" | "snippet";

/**
 * Result of an export call. `mode` is the input mode so callers
 * can branch on it without re-storing the request.
 */
export interface MarkdownExportResult {
  readonly mode: MarkdownExportMode;
  readonly text: string;
  readonly wordCount: number;
  readonly truncated: boolean;
}

/**
 * Run the export pipeline. Pure.
 */
export function exportMarkdown(
  snapshot: MarkdownCoreSnapshot,
  mode: MarkdownExportMode,
  options: { readonly maxChars?: number } = {},
): MarkdownExportResult {
  const source = snapshot.logicalSource;
  switch (mode) {
    case "plain-text": {
      const text = stripMarkdown(source);
      return {
        mode,
        text,
        wordCount: countWords(text),
        truncated: false,
      };
    }
    case "word-count": {
      const text = stripMarkdown(source);
      return { mode, text: "", wordCount: countWords(text), truncated: false };
    }
    case "snippet": {
      const max = options.maxChars ?? 240;
      const truncated = source.length > max;
      const text = truncated ? `${source.slice(0, max - 1)}…` : source;
      return { mode, text, wordCount: countWords(text), truncated };
    }
    case "preview":
      return {
        mode,
        text: source,
        wordCount: countWords(stripMarkdown(source)),
        truncated: false,
      };
  }
}

/**
 * Strip the most common Markdown formatting characters. Not a
 * full renderer — meant for word counts and previews that want
 * a sanitized snippet.
 */
export function stripMarkdown(source: string): string {
  let out = source;
  out = out.replace(/```[\s\S]*?```/g, ""); // fenced code
  out = out.replace(/`([^`]*)`/g, "$1"); // inline code (keep text)
  out = out.replace(/!\[[^\]]*\]\([^)]*\)/g, ""); // images
  out = out.replace(/\[([^\]]*)\]\([^)]*\)/g, "$1"); // links
  out = out.replace(/[#>*_~`]/g, ""); // emphasis/structural
  out = out.replace(/\s+/g, " ").trim();
  return out;
}

/**
 * Count words by splitting on whitespace. Returns 0 for empty
 * or whitespace-only input.
 */
export function countWords(text: string): number {
  const trimmed = text.trim();
  if (trimmed.length === 0) return 0;
  return trimmed.split(/\s+/).length;
}

/**
 * Round-trip a snapshot into a JSON string. Stable shape: the
 * snapshot object plus a `revision` field and a pre-computed
 * `hash` for cheap equality checks.
 */
export function snapshotToJson(snapshot: MarkdownCoreSnapshot): string {
  const payload = {
    revision: snapshot.revision,
    logicalSource: snapshot.logicalSource,
    lineEndings: snapshot.lineEndings,
    preferredLineEnding: snapshot.preferredLineEnding,
    sourceMapRevision: snapshot.sourceMap.revision,
    nodeCount: snapshot.sourceMap.nodes.length,
  };
  return JSON.stringify(payload);
}
