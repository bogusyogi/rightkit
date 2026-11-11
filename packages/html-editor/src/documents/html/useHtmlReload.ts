import {
  useCallback,
  useEffect,
  useRef,
  type Dispatch,
  type MutableRefObject,
  type SetStateAction,
} from "react";
import { sourceChecksum } from "./sourceMap.js";
import { incIframeLoads, incPreviewGenerations } from "@rightkit/document-core/documents/perfEventCounters";
import { documentEvent } from "@rightkit/document-core/host";

interface HtmlReloadDeps {
  /** Inactive keep-alive panes stay mounted but must not own global shortcut events. */
  active: boolean;
  activePath: string;
  /** Live, editable source for the active page. */
  activeBuffer: string;
  activeBufferRef: MutableRefObject<string>;
  renderedRevisionRef: MutableRefObject<string | null>;
  setApplied: Dispatch<SetStateAction<string>>;
  /** Bumped by Reload to force a preview rebuild even when bytes are unchanged. */
  setNonce: Dispatch<SetStateAction<number>>;
  /** Re-read a page from disk (refresh, browser-style). Prompts if the page has unsaved edits. */
  onReloadFromDisk: (path: string, options?: { force?: boolean }) => void;
}

/**
 * Browser-style reload: re-read the CURRENT page from disk (prompting on unsaved edits) AND force
 * a preview rebuild so scripts re-run even when the bytes are unchanged. The `reloadPendingRef`
 * guard lets the disk re-read's buffer update snapshot into `applied` without ordinary keystrokes
 * ever rebuilding the preview. Extracted from HtmlDocument verbatim — no behavior change.
 */
export function useHtmlReload({
  active,
  activePath,
  activeBuffer,
  activeBufferRef,
  renderedRevisionRef,
  setApplied,
  setNonce,
  onReloadFromDisk,
}: HtmlReloadDeps) {
  // True briefly after a Reload so that when the disk re-read lands (changing the buffer), we snapshot
  // the fresh content into `applied`. Bounded by a timeout so a later keystroke isn't mistaken for it.
  const reloadPendingRef = useRef(false);

  // Browser-style refresh: re-read the CURRENT page from disk (prompts if it has unsaved edits) AND
  // force a preview rebuild so scripts re-run even when the bytes are unchanged. Used by the Reload
  // button and by Ctrl+R / F5 (dispatched from the global shortcut handler as documentEvent("html-reload")).
  // biome-ignore lint/correctness/useExhaustiveDependencies: refs/setters arrive as stable hook params
  const doReload = useCallback(() => {
    reloadPendingRef.current = true;
    // The watcher may already have loaded newer disk bytes into the active buffer without rebuilding
    // this long-lived preview. Snapshot those bytes now, then force a fresh disk read as the authority.
    renderedRevisionRef.current = sourceChecksum(activeBufferRef.current);
    setApplied(activeBufferRef.current);
    incPreviewGenerations(1);
    incIframeLoads(1);
    onReloadFromDisk(activePath, { force: true }); // re-read disk (async; prompts if the page has unsaved edits)
    setNonce((n) => n + 1); // immediate re-run of the CURRENT content (covers the unchanged-bytes case)
    // Clear the flag if the disk read didn't change anything, so a later edit isn't treated as a reload.
    window.setTimeout(() => {
      reloadPendingRef.current = false;
    }, 1500);
  }, [onReloadFromDisk, activePath]);

  useEffect(() => {
    if (!active) return;
    window.addEventListener(documentEvent("html-reload"), doReload);
    return () => window.removeEventListener(documentEvent("html-reload"), doReload);
  }, [active, doReload]);

  // When a reload's disk re-read lands (the active buffer changes to the new on-disk content), snapshot
  // it into `applied` and bump `nonce` so the keyed iframe remounts with the fresh data. Guarded by
  // reloadPendingRef so ordinary keystrokes (which also change the buffer) never rebuild the preview.
  // biome-ignore lint/correctness/useExhaustiveDependencies: refs/setters arrive as stable hook params
  useEffect(() => {
    if (!reloadPendingRef.current) return;
    reloadPendingRef.current = false;
    renderedRevisionRef.current = sourceChecksum(activeBuffer);
    setApplied(activeBuffer);
    incPreviewGenerations(1);
    incIframeLoads(1);
    setNonce((n) => n + 1);
  }, [activeBuffer]);

  return { doReload };
}
