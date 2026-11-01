import type { ReactNode, RefObject } from "react";
import type { SearchProvider, ViewMode } from "@rightkit/document-core/documents/types";
import type { SourceEditorViewState } from "../txt/TxtEditor";
import type { HtmlPageBuffer } from "@rightkit/html-editor/documents/html/sessionTypes";

export interface HtmlDocumentProps {
  /** The originally-opened file. Its folder is the trusted site root; never changes as you navigate. */
  entryPath: string;
  /** Shared ViewMode; for HTML only interactive (default) / preview (Safe) apply now — Code is gone,
   *  replaced by the in-place source drawer below. A stale persisted "source" falls back to interactive.
   *  Flipped between interactive (scripts ON) and preview/Safe (scripts OFF) by the scripts switch in
   *  the action bar above — HtmlDocument only READS it (drives `mode`); it doesn't own the toggle. */
  view: ViewMode;
  /** Live, editable source of the ENTRY doc (owned by App via the shared edits buffer). */
  text: string;
  onChange: (text: string) => void;
  registerSearchProvider: (provider: SearchProvider | null) => void;
  findBar?: ReactNode;
  findMode?: "find" | "replace";
  onRequestFind?: () => void;
  onRequestReplace?: () => void;
  /** Close the shared find bar. The drawer and the preview search DIFFERENT surfaces, so an open
   *  find must not silently survive the switch between them. */
  onCloseFind?: () => void;
  /** Open a linked doc as its own the host app tab. `background` (new-tab gesture) adds it without
   *  switching away from the current document. */
  onOpenPath?: (path: string, opts?: { background?: boolean }) => void;
  /** Switch Interactive edit requests into Safe before creating a rich-edit draft. */
  onChangeView?: (view: ViewMode) => void;
  /** Inactive keep-alive panes stay mounted but must not own global shortcut events. */
  active?: boolean;
  /** Wave 2 Read mode: the source drawer, block editing, and inspect-to-source all stay closed —
   *  Interactive/Safe `view` and every viewing control are unaffected. */
  readOnly?: boolean;

  // ── HTML site session (sub-page buffers live at App level; see useDocumentTabs) ──
  /** Editable buffers for navigated (non-entry) pages in this session, keyed by absolute path. */
  pageBuffers: Record<string, HtmlPageBuffer>;
  /** Absolute paths of every dirty page in this session (entry + subs) — drives the page dirty marker. */
  dirtyPagePaths: string[];
  /** Seed a sub-page buffer from disk on first visit (no-op for the entry / already-loaded page). */
  onEnsurePage: (entryPath: string, pagePath: string) => void;
  /** Edit a page's buffer (entry routes to onChange; subs to App's per-page buffer). */
  onPageChange: (entryPath: string, pagePath: string, text: string) => void;
  /** Report the in-site page currently shown (App needs it for Ctrl+S targeting). */
  onSetActivePage: (entryPath: string, pagePath: string) => void;
  /** Save one page to disk (entry or sub). */
  onSavePage: (entryPath: string, pagePath: string) => void;
  /** Revert one page's buffer to its on-disk content. */
  onDiscardPage: (entryPath: string, pagePath: string) => void;
  /** Re-read a page from disk (refresh, browser-style). Prompts if the page has unsaved edits. */
  onReloadFromDisk: (path: string, options?: { force?: boolean }) => void;
  /** Durable preview scroll: seeded on mount so scroll survives a tab-switch unmount. */
  initialPreviewScrollY?: number;
  /** Report the live preview scroll up to App's per-tab store (so it's restored on return). */
  onPreviewScroll?: (y: number) => void;
  /** App-owned source selection/scroll, retained if this dirty session is shelled. */
  sourceViewStateRef?: RefObject<Record<string, SourceEditorViewState>>;
}
