/**
 * TS-side wrapper for the Rust folder-index command.
 *
 * The Rust side enforces the cap rules (1 000 files, 20 MiB total,
 * 2 MiB per file, depth 8) and excludes hidden directories and
 * build outputs. The TS layer:
 *
 *  - shapes the request and the response,
 *  - caches the most recent index per document for the duration
 *    of the session so the picker does not re-call Rust when the
 *    user reopens the picker,
 *  - exposes typed errors for the four known failure modes
 *    (`no-document-parent`, `too-many-files`, `too-many-bytes`,
 *    `too-deep`).
 */

export interface MarkdownIndexedFile {
  readonly absolutePath: string;
  readonly relativePath: string;
  readonly byteLength: number;
  readonly title: string | null;
}

export type FolderIndexErrorCode =
  | "no-document-parent"
  | "too-many-files"
  | "too-many-bytes"
  | "too-deep";

export class FolderIndexError extends Error {
  constructor(
    readonly code: FolderIndexErrorCode,
    message: string,
  ) {
    super(message);
    this.name = "FolderIndexError";
  }
}

/**
 * Folder-index client interface. The Rust backend implements this
 * via `index_markdown_folder`. Tests inject a fake client.
 */
export interface FolderIndexClient {
  index(
    documentPath: string,
    recursive: boolean,
    explicitConsent: true,
  ): Promise<readonly MarkdownIndexedFile[]>;
}

/** Native folder walks require an explicit action from a user-visible picker. */
export class NativeFolderIndexClient implements FolderIndexClient {
  async index(
    documentPath: string,
    recursive: boolean,
    explicitConsent: true,
  ): Promise<readonly MarkdownIndexedFile[]> {
    const { invoke } = await import("@tauri-apps/api/core");
    const result = await invoke<{ files: MarkdownIndexedFile[] }>("index_markdown_folder", {
      documentPath,
      recursive,
      explicitConsent,
    });
    return result.files;
  }
}

/**
 * Process-level cache of the most recent index, keyed by the
 * canonical document parent. The session expires when the tab is
 * closed; for long-lived sessions the caller invalidates the
 * cache via {@link FolderIndexCache.invalidate}.
 */
export class FolderIndexCache {
  private byParent = new Map<
    string,
    { files: readonly MarkdownIndexedFile[]; fetchedAt: number }
  >();
  constructor(private readonly client: FolderIndexClient) {}

  async fetch(
    documentPath: string,
    recursive: boolean,
    explicitConsent: true,
  ): Promise<readonly MarkdownIndexedFile[]> {
    const parent = parentOf(documentPath);
    if (!parent) {
      throw new FolderIndexError("no-document-parent", "Document path has no parent directory.");
    }
    const cached = this.byParent.get(parent);
    if (cached) return cached.files;
    const files = await this.client.index(documentPath, recursive, explicitConsent);
    this.byParent.set(parent, { files, fetchedAt: Date.now() });
    return files;
  }

  invalidate(documentPath: string): void {
    const parent = parentOf(documentPath);
    if (parent) this.byParent.delete(parent);
  }

  clear(): void {
    this.byParent.clear();
  }
}

function parentOf(path: string): string | null {
  const i = path.lastIndexOf("/");
  if (i <= 0) return null;
  return path.slice(0, i);
}

/**
 * Filter an index by a free-text query. The query matches the
 * relative path and the title (case-insensitive).
 */
export function filterFolderIndex(
  files: readonly MarkdownIndexedFile[],
  query: string,
): readonly MarkdownIndexedFile[] {
  const needle = query.trim().toLowerCase();
  if (needle.length === 0) return files;
  return files.filter((f) => {
    const path = f.relativePath.toLowerCase();
    const title = (f.title ?? "").toLowerCase();
    return path.includes(needle) || title.includes(needle);
  });
}
