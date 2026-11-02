import {
  useEffect,
  useRef,
  type Dispatch,
  type MutableRefObject,
  type SetStateAction,
} from "react";
import { buildInteractivePreviewHtml } from "./htmlInteractive.js";
import type { SiteRootGrant } from "./htmlInline.js";
import { logEvent } from "@rightkit/document-core/state/diagnostics";
import { errorMessage } from "@rightkit/document-core/state/errors";
import type { HtmlPageBuffer } from "./sessionTypes.js";
import { sourceChecksum } from "./sourceMap.js";
import { htmlSrcDocCache, trackCancelledHtmlBuild } from "./htmlResourceLifecycle.js";
import { incIframeLoads, incPreviewGenerations } from "@rightkit/document-core/documents/perfEventCounters";
import type { HtmlMutationPlan } from "./htmlMutationEngine.js";

/** Page-scoped history entry: the exact committed plan plus its exact inverse. */
export interface HtmlBlockHistoryEntry {
  pagePath: string;
  /** Forward plan, valid at its committed revision (redo). */
  plan: HtmlMutationPlan;
  /** Exact inverse plan, valid at the committed revision (undo). */
  inversePlan: HtmlMutationPlan;
}

function planTextStillAtRange(source: string, plan: HtmlMutationPlan): boolean {
  // Live-typing heuristic: a committed plan's replacement text is still present at its
  // recorded ranges, so the buffer change is a continuation, not an external replacement.
  return plan.patches.every((patch) => {
    const start = patch.range.start;
    return source.slice(start, start + patch.text.length) === patch.text;
  });
}

interface HtmlPreviewLifecycleDeps {
  /** Inactive keep-alive panes stay mounted but must not rebuild the preview. */
  active: boolean;
  mode: "interactive" | "preview";
  entryPath: string;
  activePath: string;
  /** `activePath === entryPath` — entry vs sub-page buffer ownership. */
  onEntry: boolean;
  /** False while a navigated sub-page's buffer is still loading from disk. */
  subLoaded: boolean;
  drawerOpen: boolean;
  /** The applied snapshot the preview currently renders. */
  applied: string;
  /** Live, editable source for the active page. */
  activeBuffer: string;
  /** Bumped by Reload to force a rebuild even when bytes are unchanged. */
  nonce: number;
  sessionId: string;
  /** undefined while the grant is still resolving — never build user HTML against tauri:// yet. */
  siteRoot: SiteRootGrant | null | undefined;
  initialAppliedReady: boolean;
  pendingFragment: string | null;
  pageBuffers: Record<string, HtmlPageBuffer>;
  activeBufferRef: MutableRefObject<string>;
  blockHistoryRef: MutableRefObject<HtmlBlockHistoryEntry[]>;
  redoHistoryRef: MutableRefObject<HtmlBlockHistoryEntry[]>;
  renderedRevisionRef: MutableRefObject<string | null>;
  scrollYRef: MutableRefObject<number>;
  mountedRef: MutableRefObject<boolean>;
  setApplied: Dispatch<SetStateAction<string>>;
  setSrcDoc: Dispatch<SetStateAction<string>>;
  setError: Dispatch<SetStateAction<string | null>>;
  setInitialAppliedReady: Dispatch<SetStateAction<boolean>>;
}

/**
 * The preview-build lifecycle: snapshot the active page's buffer into `applied` only when the
 * rendered revision actually advances (navigation, sub-page load, external replacement, discard —
 * never live typing), then build the sandboxed interactive srcDoc from that snapshot. Extracted
 * from HtmlDocument verbatim — no behavior change.
 */
export function useHtmlPreviewLifecycle({
  active,
  mode,
  entryPath,
  activePath,
  onEntry,
  subLoaded,
  drawerOpen,
  applied,
  activeBuffer,
  nonce,
  sessionId,
  siteRoot,
  initialAppliedReady,
  pendingFragment,
  pageBuffers,
  activeBufferRef,
  blockHistoryRef,
  redoHistoryRef,
  renderedRevisionRef,
  scrollYRef,
  mountedRef,
  setApplied,
  setSrcDoc,
  setError,
  setInitialAppliedReady,
}: HtmlPreviewLifecycleDeps) {
  const pageBuffersRef = useRef(pageBuffers);
  pageBuffersRef.current = pageBuffers;

  // Snapshot the active page's current buffer into `applied` when (and only when) the preview should
  // reload. Ownership is renderedRevision (sourceChecksum): preview reloads only when checksum
  // differs from renderedRevisionRef. Navigation, sub-page
  // load, external replacement, and explicit full-source changes are detected via activePath/
  // subLoaded/activeBuffer changes. Live typing while drawerOpen is handled by the debounced
  // live-preview effect; this effect is gated to avoid double counting.
  // biome-ignore lint/correctness/useExhaustiveDependencies: reload ownership is the source checksum; refs are read on purpose
  useEffect(() => {
    const next = activeBufferRef.current;
    const checksum = sourceChecksum(next);
    // External full-source replacement invalidates prior inverse patches — clear even while
    // drawer is open (stale draft handling also clears, but this covers the no-draft case).
    // Do not clear on every live keystroke: only when the new checksum diverges from the
    // last rendered revision AND the buffer change is not just debounced live typing.
    // Live typing while drawerOpen is handled by the debounced effect; we gate preview
    // update below but still clear history if this is a genuine external replacement.
    // Heuristic: if drawerOpen and checksum differs, treat as external only if history exists
    // and the new source does not contain the last committed text at its recorded range.
    if (checksum !== renderedRevisionRef.current) {
      const hasHistory = blockHistoryRef.current.some((e) => e.pagePath === activePath);
      const hasRedo = redoHistoryRef.current.some((e) => e.pagePath === activePath);
      if (hasHistory || hasRedo) {
        // If drawer is open, only clear if the change looks external (not live typing that
        // still contains the last edits). Check if last history entry's text is still present.
        let shouldClear = true;
        if (drawerOpen && hasHistory) {
          const last = [...blockHistoryRef.current]
            .reverse()
            .find((e) => e.pagePath === activePath);
          if (last && planTextStillAtRange(next, last.plan)) {
            // If current buffer still contains the last committed replacement text at its recorded
            // ranges, this is likely just live typing after a Done, not an external replacement.
            shouldClear = false;
          }
        }
        if (shouldClear) {
          if (hasHistory) {
            blockHistoryRef.current = blockHistoryRef.current.filter(
              (e) => e.pagePath !== activePath,
            );
          }
          if (hasRedo) {
            redoHistoryRef.current = redoHistoryRef.current.filter(
              (e) => e.pagePath !== activePath,
            );
          }
        }
      }
    }
    // Live editing while drawer is open is handled by the debounced live-preview effect;
    // do not snapshot here while drawer is open.
    if (drawerOpen) return;
    if (checksum === renderedRevisionRef.current) return;
    renderedRevisionRef.current = checksum;
    setApplied(next);
    // Only count a preview generation when the applied snapshot actually
    // advances to a new rendered revision (not on initial mount where
    // the boot html is still empty). The block-commit path already
    // counted its generation explicitly via incPreviewGenerations above,
    // so skip double-counting when that path just set the same checksum.
    if (next !== "") {
      // This path is for navigation / external / discard. Block commit already bumped for its
      // revision and set renderedRevisionRef synchronously, so this effect's early-return above
      // prevents double counting. Any checksum advance reaching here is a legitimate generation.
      incPreviewGenerations(1);
      incIframeLoads(1);
    }
    setInitialAppliedReady(true);
  }, [activePath, onEntry, subLoaded, activeBuffer, drawerOpen]);

  // Build the sandboxed interactive srcDoc from the APPLIED snapshot (not the live buffer).
  const prevActivePathRef = useRef<string | null>(null);
  // biome-ignore lint/correctness/useExhaustiveDependencies: refs/setters arrive as stable hook params
  useEffect(() => {
    if (!active || mode !== "interactive" || !initialAppliedReady || siteRoot === undefined) return;
    const prevActivePath = prevActivePathRef.current;
    prevActivePathRef.current = activePath;
    // Tab switches change activePath without changing what the iframe should display. Skip those
    // so the iframe's keep-alive visibility:hidden actually keeps the iframe alive. Without this
    // guard, every click on a different tab fires buildInteractivePreviewHtml (DOMParser, script
    // inlining with per-script Rust IPC, DOM construction) and then setSrcDoc — reloading scripts
    // and resetting scroll on every switch. Within-doc navigation (prev and new both in this
    // entry or its sub-pages) is a real rebuild and still fires.
    // "In this doc" = activePath is the entry, OR a known sub-page (in pageBuffers).
    const inThisDoc = (p: string | null) =>
      p !== null && (p === entryPath || p in pageBuffersRef.current);
    const wasInThisDoc = inThisDoc(prevActivePath);
    const isNowInThisDoc = inThisDoc(activePath);
    if (prevActivePath !== null && wasInThisDoc !== isNowInThisDoc) return;
    let cancelled = false;
    const appliedAtBuild = applied;
    // Interactive srcDoc embeds this instance's session id, so never reuse its bytes across mounts.
    const cacheKey = `${entryPath}\u0000${activePath}\u0000${sessionId}\u0000${nonce}\u0000${siteRoot?.token ?? ""}\u0000${sourceChecksum(applied)}`;
    const cached = htmlSrcDocCache.get(cacheKey);
    if (cached) {
      setSrcDoc(cached);
      return;
    }
    // Diagnostic: an iframe (re)build = scripts re-run + scroll reset (the "reload"). Logged so a tab
    // switch that triggers one is visible in Settings → Diagnostics (it should NOT fire on switch).
    logEvent("html:build");
    setError(null);
    void buildInteractivePreviewHtml(
      applied,
      activePath,
      sessionId,
      entryPath,
      pendingFragment ?? undefined,
      scrollYRef.current,
      siteRoot,
    )
      .then((built) => {
        if (cancelled || !mountedRef.current) {
          trackCancelledHtmlBuild();
          return;
        }
        // Stale async renders cannot replace newer revision: if applied changed while this
        // async build was in flight, discard the result.
        if (appliedAtBuild !== applied) return;
        htmlSrcDocCache.set(cacheKey, built);
        setSrcDoc(built);
      })
      .catch((e) => {
        if (!cancelled && mountedRef.current && appliedAtBuild === applied)
          setError(errorMessage(e));
      });
    return () => {
      cancelled = true;
    };
  }, [
    mode,
    active,
    initialAppliedReady,
    applied,
    activePath,
    sessionId,
    entryPath,
    siteRoot,
    nonce,
    pendingFragment,
  ]);
}
