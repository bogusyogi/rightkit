/**
 * Source-preserving frontmatter document.
 *
 * The frontmatter block at the top of a Markdown file is YAML.
 * We use `yaml` package's `parseDocument` so comments, key order,
 * and scalar style are preserved across reads and writes.
 *
 * The contract:
 *
 *  - callers always go through {@link readFrontmatterDocument} or
 *    {@link writeFrontmatterDocument} to read/write the block,
 *  - reads return a `{ parsed, raw, range, isMissing }` shape,
 *  - writes round-trip the source exactly when the document was
 *    untouched, and emit minimal diffs when properties change.
 */

import type { Document } from "yaml";
import { parseDocument } from "yaml";

/**
 * Half-open logical-source range of the frontmatter block, when
 * present. `null` when the file does not start with `---\n`.
 */
export interface FrontmatterRange {
  readonly from: number;
  readonly to: number;
}

export interface FrontmatterDocument {
  readonly parsed: Document.Parsed | null;
  readonly raw: string;
  readonly range: FrontmatterRange | null;
  readonly isMissing: boolean;
}

/**
 * Read the frontmatter from `source`. Returns `isMissing: true`
 * when there is no frontmatter block. Never throws.
 */
export function readFrontmatterDocument(source: string): FrontmatterDocument {
  const startMatch = /^---\r?\n/.exec(source);
  if (!startMatch) {
    return {
      parsed: null,
      raw: "",
      range: null,
      isMissing: true,
    };
  }
  const start = startMatch.index;
  const bodyStart = start + startMatch[0].length;
  // Find the closing `---` line.
  const closing = findClosingFence(source, bodyStart);
  if (closing === -1) {
    // Unclosed; treat as missing.
    return {
      parsed: null,
      raw: "",
      range: null,
      isMissing: true,
    };
  }
  const raw = source.slice(bodyStart, closing);
  const parsed = parseDocument(raw);
  const range: FrontmatterRange = { from: start, to: closing + 4 };
  return {
    parsed: parsed as Document.Parsed | null,
    raw,
    range,
    isMissing: false,
  };
}

/**
 * Find the closing `---` (or `...`) fence. Returns the offset of
 * the `\n` immediately before the closing fence, or -1 if not
 * found.
 */
function findClosingFence(source: string, bodyStart: number): number {
  const regex = /\r?\n---\r?\n|\r?\n\.\.\.\r?\n/g;
  regex.lastIndex = bodyStart;
  const m = regex.exec(source);
  if (!m) return -1;
  const closing = m.index + m[0].length - (m[0].endsWith("\n") ? 1 : 0) - 3;
  return closing;
}

/**
 * Serialize the parsed document back to YAML. The round-trip
 * preserves the scalar style and key order recorded by
 * `parseDocument`.
 */
export function stringifyFrontmatterDocument(doc: Document.Parsed): string {
  return doc.toString();
}

/**
 * Replace the frontmatter block in `source` with the supplied
 * YAML string. The replacement keeps the original fence style
 * (LF or CRLF) by inspecting the source's first newline.
 */
export function writeFrontmatterDocument(source: string, yamlText: string): string {
  const existing = readFrontmatterDocument(source);
  const newline = /\r\n/.test(source) ? "\r\n" : "\n";
  const block = `---\n${yamlText.replace(/\r?\n/g, newline)}${newline}---${newline}`;
  if (existing.isMissing || existing.range === null) {
    return `${block}${source}`;
  }
  return `${source.slice(0, existing.range.from)}${block}${source.slice(existing.range.to)}`;
}
