/**
 * Session-wide markdown file index.
 *
 * Tracks every markdown document the user has opened during the
 * session, regardless of whether its parent folder is on disk.
 * The session index powers the link picker's "recent" section.
 *
 * The session is bounded: the older half of the index is evicted
 * when the count exceeds {@link MAX_SESSION_INDEX_ENTRIES}.
 */

import type { MarkdownIndexedFile } from "./folderIndex";

export const MAX_SESSION_INDEX_ENTRIES = 256;

/**
 * One session entry. The `source` describes where the entry came
 * from — useful for debugging and for the picker UI.
 */
export interface SessionMarkdownEntry {
  readonly file: MarkdownIndexedFile;
  readonly source: "recent" | "pinned" | "transient";
  readonly lastOpenedAt: number;
}

export class MarkdownSessionIndex {
  private byPath = new Map<string, SessionMarkdownEntry>();
  private insertionOrder: string[] = [];

  add(file: MarkdownIndexedFile, source: SessionMarkdownEntry["source"]): void {
    const existing = this.byPath.get(file.absolutePath);
    const entry: SessionMarkdownEntry = {
      file,
      source,
      lastOpenedAt: Date.now(),
    };
    this.byPath.set(file.absolutePath, entry);
    if (!existing) {
      this.insertionOrder.push(file.absolutePath);
    }
    this.evict();
  }

  remove(absolutePath: string): void {
    this.byPath.delete(absolutePath);
    const idx = this.insertionOrder.indexOf(absolutePath);
    if (idx >= 0) this.insertionOrder.splice(idx, 1);
  }

  promote(absolutePath: string, source: SessionMarkdownEntry["source"]): void {
    const existing = this.byPath.get(absolutePath);
    if (!existing) return;
    const entry: SessionMarkdownEntry = { ...existing, source, lastOpenedAt: Date.now() };
    this.byPath.set(absolutePath, entry);
  }

  list(): readonly SessionMarkdownEntry[] {
    return Array.from(this.byPath.values()).sort((a, b) => b.lastOpenedAt - a.lastOpenedAt);
  }

  clear(): void {
    this.byPath.clear();
    this.insertionOrder = [];
  }

  private evict(): void {
    while (this.insertionOrder.length > MAX_SESSION_INDEX_ENTRIES) {
      const victim = this.insertionOrder.shift();
      if (victim) this.byPath.delete(victim);
    }
  }
}
