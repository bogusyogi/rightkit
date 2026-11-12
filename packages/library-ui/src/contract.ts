// The library contract. The list and reader hold no IPC: the host implements `LibrarySource` over its own
// store (HeardRight's delivery history, ScrapeRight's artifacts, ...) and maps its records to
// `LibraryRow` and `LibraryDetail`. Every capability past `query` and `copyText` is optional; the
// list and reader hide what the source does not offer.

import type { ReactNode } from "react";

export type Unsubscribe = () => void;

/** Where a row came from: an app, a file, a synced device. */
export interface LibraryOrigin {
  /** Shown in the meta line and as the origin filter's option text. */
  label: string;
  /**
   * The source's own key for this origin (e.g. a raw process name), sent as `LibraryQuery.origin` when the
   * origin filter picks it. Defaults to `label`; set it whenever `label` is a display form the source cannot
   * match on.
   */
  filterKey?: string;
  /** Extra detail (a window title) shown only when it adds information. */
  detail?: string;
  /** Key the source resolves to an icon (`LibrarySource.originIcon`). */
  iconKey?: string;
  /** Fallback glyph when no icon resolves. */
  kind?: "app" | "file" | "synced";
}

export interface LibraryRow {
  id: string;
  /** The text the row shows and copies. */
  text: string;
  /** A second version of the text (e.g. before AI polish); the row offers a toggle when it differs. */
  altText?: string | null;
  timestampMs: number;
  origin?: LibraryOrigin;
  /** Short state label after the origin ("Pasted", "Saved"). */
  status?: string;
  /** Neutral tags before the meta line ("Cancelled"). */
  badges?: string[];
  /** Media length, shown as m:ss. */
  durationMs?: number | null;
  /** Group key (e.g. the device a row came from); matches a `LibraryGroup.key`. */
  group?: string;
  /** True when this window may not edit, select or delete the row (e.g. synced from another device). */
  readOnly?: boolean;
}

export interface LibraryQuery {
  search: string;
  fromMs: number | null;
  untilMs: number | null;
  /** The picked origin's `filterKey` (its `label` when it has none), or "" for all. */
  origin: string;
  /** Active variant key (see `LibraryVariant`). */
  variant: string;
  offset: number;
  limit: number;
}

export interface LibraryPageResult {
  rows: LibraryRow[];
  total: number;
  hasMore: boolean;
}

/** A tab over the list (e.g. "Dictations" / "Cancelled"); the source scopes the query by `variant`. */
export interface LibraryVariant {
  key: string;
  label: string;
  /** Empty state when no row matches this variant. */
  emptyTitle?: string;
  emptyBody?: string;
}

/** A group rows belong to (e.g. a device). Groups the viewer switches off are hidden client-side. */
export interface LibraryGroup {
  key: string;
  label: string;
  icon?: ReactNode;
  /** On when nothing is stored yet. */
  defaultOn: boolean;
  /** Rows of this group get no badge (the local device). */
  local?: boolean;
}

export interface LibraryRetention {
  options: { value: string; label: string }[];
  get(): Promise<string>;
  /** Stores the policy and returns how many rows it removed. */
  set(value: string): Promise<number>;
}

/** A store the source had to set aside (kept, not deleted). Dismissing hides it until a newer one. */
export interface LibraryRecoveryStatus {
  createdAtMs: number;
  title: string;
  message: string;
}

export interface LibraryRecovery {
  status(): Promise<LibraryRecoveryStatus | null>;
  /** Shows a save dialog and writes diagnostics; resolves false when cancelled. */
  exportDiagnostics(): Promise<boolean>;
}

/** One reader view of a detail (plain, timed, Markdown, subtitles). */
export type LibraryDetailView = "txt" | "txtTime" | "md" | "mdTime" | "srt";

export interface LibraryDetail {
  id: string;
  title: string;
  /** Plain text; always present. */
  text: string;
  timedText?: string | null;
  markdown?: string | null;
  timedMarkdown?: string | null;
  srt?: string | null;
  /** Playable media URL the host resolved (the package does not convert file paths). */
  mediaUrl?: string | null;
  mediaKind?: "audio" | "video" | null;
  /** Image URLs (e.g. a carousel) the host resolved. */
  images?: string[];
  /** Original location (a web URL), shown as a link. */
  sourceUrl?: string | null;
}

export interface LibrarySource {
  query(query: LibraryQuery): Promise<LibraryPageResult>;
  /** Calls `onChange` whenever stored rows change (a new delivery, a sync pull). */
  subscribe?(onChange: () => void): Promise<Unsubscribe>;
  copyText(text: string): Promise<void>;
  remove?(id: string): Promise<void>;
  /** Replaces a row's text; resolves false when the row no longer exists. */
  update?(id: string, text: string): Promise<boolean>;
  /** Exports everything (the host shows its own dialog); resolves the count, or null when cancelled. */
  exportAll?(): Promise<number | null>;
  /** Shows a save dialog and writes `contents`; resolves false when cancelled. */
  saveText?(suggestedName: string, contents: string): Promise<boolean>;
  retention?: LibraryRetention;
  recovery?: LibraryRecovery;
  /** Icon (an image URL) for an origin, or null. Results are cached per `iconKey`. */
  originIcon?(origin: LibraryOrigin): Promise<string | null>;
  detail?(id: string): Promise<LibraryDetail>;
}
