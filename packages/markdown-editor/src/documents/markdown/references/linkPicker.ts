/**
 * Link picker state machine.
 *
 * Given the index (folder or session), a query, and a current
 * selection, returns the list of {@link LinkPickerEntry}s the UI
 * should render. The picker supports two modes:
 *
 *  - `link`: pick a Markdown file path. The result is the
 *    relative path from the document's parent directory.
 *  - `anchor`: pick a heading in the current document. The
 *    result is the heading slug with a leading `#`.
 *
 * The picker is *pure*: it never reads from React state or DOM
 * directly. Callers pass in the current selection and the
 * available index.
 */

import type { MarkdownIndexedFile } from "./folderIndex";
import type { MarkdownSourceNode } from "../core/MarkdownSourceMap";

export type LinkPickerKind = "link" | "anchor";

export interface LinkPickerEntry {
  readonly key: string;
  readonly displayPath: string;
  readonly displayTitle: string;
  readonly insertText: string;
  readonly kind: LinkPickerKind;
}

export interface LinkPickerInput {
  readonly kind: LinkPickerKind;
  readonly query: string;
  readonly folderFiles: readonly MarkdownIndexedFile[];
  readonly sessionFiles: readonly MarkdownIndexedFile[];
  readonly currentDocumentPath: string | null;
  readonly headings: readonly MarkdownSourceNode[];
}

const HEADING_QUERY_LIMIT = 40;

/**
 * Build the picker entries. The first section is the *recent*
 * section, sourced from the session index; the second section is
 * the *folder* section, sourced from the folder index. For
 * `anchor` kind, only the heading section is returned.
 */
export function buildLinkPickerEntries(input: LinkPickerInput): readonly LinkPickerEntry[] {
  if (input.kind === "anchor") {
    return buildAnchorEntries(input);
  }
  return buildFileEntries(input);
}

function buildFileEntries(input: LinkPickerInput): readonly LinkPickerEntry[] {
  const needle = input.query.trim().toLowerCase();
  const session: LinkPickerEntry[] = [];
  const folder: LinkPickerEntry[] = [];
  const seen = new Set<string>();
  for (const file of input.sessionFiles) {
    const key = file.absolutePath;
    if (seen.has(key)) continue;
    seen.add(key);
    if (matchesQuery(file, needle)) {
      session.push({
        key,
        displayPath: file.relativePath,
        displayTitle: file.title ?? file.relativePath,
        insertText: relativeLinkPath(
          input.currentDocumentPath,
          file.absolutePath,
          file.relativePath,
        ),
        kind: "link",
      });
    }
  }
  for (const file of input.folderFiles) {
    if (seen.has(file.absolutePath)) continue;
    if (matchesQuery(file, needle)) {
      folder.push({
        key: file.absolutePath,
        displayPath: file.relativePath,
        displayTitle: file.title ?? file.relativePath,
        insertText: relativeLinkPath(
          input.currentDocumentPath,
          file.absolutePath,
          file.relativePath,
        ),
        kind: "link",
      });
    }
  }
  return [...session, ...folder];
}

function buildAnchorEntries(input: LinkPickerInput): readonly LinkPickerEntry[] {
  const needle = input.query.trim().toLowerCase();
  const out: LinkPickerEntry[] = [];
  for (const heading of input.headings) {
    if (out.length >= HEADING_QUERY_LIMIT) break;
    const slug = heading.headingId ?? "";
    const text = slug.toLowerCase();
    if (
      needle.length > 0 &&
      !text.includes(needle) &&
      !heading.semanticKey.toLowerCase().includes(needle)
    ) {
      continue;
    }
    out.push({
      key: heading.id,
      displayPath: `#${slug}`,
      displayTitle: heading.semanticKey,
      insertText: `#${slug}`,
      kind: "anchor",
    });
  }
  return out;
}

function matchesQuery(file: MarkdownIndexedFile, needle: string): boolean {
  if (needle.length === 0) return true;
  return (
    file.relativePath.toLowerCase().includes(needle) ||
    (file.title ?? "").toLowerCase().includes(needle)
  );
}

function relativeLinkPath(
  documentPath: string | null,
  absolutePath: string,
  relativePath: string,
): string {
  if (!documentPath) return relativePath;
  // For now, the picker always returns the relative path the Rust
  // side already computed. A more sophisticated path-comparison
  // could shorten `../foo/bar.md` further; deferred.
  void absolutePath;
  return relativePath;
}
