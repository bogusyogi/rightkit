import type { FileFingerprint } from "@rightkit/document-core/documents/types";

/** One editable source buffer for a single local HTML page within a site session. */
export interface HtmlPageBuffer {
  /** Last on-disk content; a clean buffer has edited === original. */
  original: string;
  /** Live, editable content. */
  edited: string;
  /** Monotonic in-session revision owned by this page buffer (source checksum remains fallback). */
  revision?: number;
  /** Encoding the page was read as, preserved on save. */
  encoding: string;
  /** On-disk identity captured at read/save time for conflict detection. */
  fingerprint?: FileFingerprint;
  /** SHA-256 source identity checked by backend immediately before save. */
  sourceSha256?: string;
  /** Set when the file changed on disk while this page had unsaved edits; the draft is based on older bytes. */
  stale?: boolean;
}

/** entryPath -> pagePath -> editable buffer for one local-site session. */
export type HtmlSessionPages = Record<string, Record<string, HtmlPageBuffer>>;
