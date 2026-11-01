/**
 * Opaque, monotonic node ID factory.
 *
 * IDs are minted in source order during candidate allocation. The
 * `documentId` is a per-document random prefix chosen when the core
 * is mounted; tests inject a deterministic value so IDs are
 * reproducible across runs.
 *
 * IDs must never be derived from paths, source text, titles, hashes,
 * or any user data — only the factory's counter may contribute.
 */

export type MarkdownNodeId = string & { readonly __brand: "MarkdownNodeId" };

/**
 * Wrap a literal ID string in the branded type. Callers must only
 * use values returned by the factory.
 */
export function asMarkdownNodeId(value: string): MarkdownNodeId {
  return value as MarkdownNodeId;
}

/**
 * Construct an ID literal from `documentId` and `n`. The format is
 * `mdn:<documentId>:<n>`. Exposed for the factory's internal use and
 * for tests asserting ID format.
 */
export function formatMarkdownNodeId(documentId: string, n: number): MarkdownNodeId {
  return asMarkdownNodeId(`mdn:${documentId}:${n}`);
}

/**
 * Sequential ID factory. Created once per document mount.
 */
export class MarkdownNodeIdFactory {
  private next = 1;
  private readonly documentId: string;

  constructor(documentId: string) {
    this.documentId = documentId;
  }

  /**
   * Mints the next ID. IDs are never reissued, even across
   * reconciliation passes.
   */
  create(): MarkdownNodeId {
    return formatMarkdownNodeId(this.documentId, this.next++);
  }

  /**
   * Peek at the next ID without consuming it. Useful for tests.
   */
  peek(): MarkdownNodeId {
    return formatMarkdownNodeId(this.documentId, this.next);
  }

  /**
   * Document-prefix portion. Tests use this to build expected IDs.
   */
  get prefix(): string {
    return this.documentId;
  }
}
