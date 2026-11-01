// Horizontal open-document tab strip for the fused titlebar (Part A of the
// tabs+explorer redesign — see docs/design/viewright-tabs-explorer-brief.md and the
// visual spec docs/design/viewright-tabs-explorer.html). `docs` (App's openDocs order)
// IS the tab order — the strip is a view of that state, never a second source of truth.
// The active tab fuses into the page with the concave bottom-corner curves from
// TabFuseCurves; inactive tabs sit on the chrome band with quiet hairline separators.
// Styles: src/styles/tabs.css (imported below so the strip carries its own CSS).
import { tabDomId } from "./tabIds.js";
import {
  memo,
  useCallback,
  useEffect,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
  type MouseEvent as ReactMouseEvent,
  type PointerEvent as ReactPointerEvent,
  type ReactNode,
  type WheelEvent as ReactWheelEvent,
} from "react";
import { CaretLeft } from "@phosphor-icons/react/dist/icons/CaretLeft";
import { CaretRight } from "@phosphor-icons/react/dist/icons/CaretRight";
import { Cards } from "@phosphor-icons/react/dist/icons/Cards";
import { X } from "@phosphor-icons/react/dist/icons/X";
import type { LoadedDocument } from "@rightkit/document-core/documents/types";
import { isUntitled } from "@rightkit/document-core/documents/types";
import { FileTypeIcon } from "../../documents/FileTypeIcon.js";
import { shortcutLabel, isMac } from "@rightkit/document-core/platform";
import { LiveTitle } from "../LiveTitle.js";
import { useTabDirty } from "./useTabDirty.js";
import { DocumentMenu } from "../sidebar/SidebarDocumentMenu.js";
import { TabFuseCurves } from "./TabFuseCurves.js";
import { TabOverview } from "./TabOverview.js";

/**
 * Everything the strip needs, supplied by the shell (App builds this once so the
 * memoized strip skips per-keystroke re-renders the same way Sidebar/Toolbar do).
 */
export interface TabStripProps {
  /** openDocs in order — single source of truth for tab order. */
  docs: LoadedDocument[];
  activePath: string | null;
  sidebarCollapsed: boolean;
  /** Dispatches "viewright:toggle-sidebar" (the sidebar owns the actual state). */
  onToggleSidebar: () => void;
  onActivate: (path: string) => void;
  /** Goes through the existing dirty-confirm flow (requestClose). */
  onRequestClose: (path: string) => void;
  onCloseAll: () => void;
  onCloseOthers: (path: string) => void;
  onCloseRight: (path: string) => void;
  onReorderTabs: (fromPath: string, toPath: string, position?: "before" | "after") => void;
  /** New untitled txt buffer (Ctrl/Cmd+T). */
  onNewDocument: () => void;
  onTogglePin: (path: string) => void;
  pinnedPaths: ReadonlySet<string>;
  openLocalPaths: string[];
  titleFor: (doc: LoadedDocument) => string;
  isDirty: (doc: LoadedDocument) => boolean;
  dirtyPaths: ReadonlySet<string>;
  subscribeDirty: (path: string, cb: () => void) => () => void;
  subscribeEdits: (path: string, cb: () => void) => () => void;
  getEditSnapshot: (path: string) => string | undefined;
  /** LiveTitle compute — keeps untitled tab names live while typing. */
  computeTitle: (doc: LoadedDocument) => string;
  overviewOpen: boolean;
  onOverviewOpenChange: (open: boolean) => void;
  /** Bulk-close an arbitrary subset (integrator wires closeMany — the overview's
   *  "Close all" footer uses it; dirty docs still go through the unsaved-changes
   *  chooser). */
  onClosePaths: (paths: string[]) => void;
}

/** Pointer distance (px) before a tab press becomes a drag-reorder. */
const DRAG_THRESHOLD = 4;

type PointerTabDrag = {
  element: HTMLElement;
  path: string;
  pointerId: number;
  startX: number;
  startY: number;
  moved: boolean;
};

type TabDropTarget = { toPath: string; position: "before" | "after" } | null;

// Memoized for the same reason Sidebar/Toolbar are: App re-renders far more often than
// the strip's inputs change (every keystroke). All props arrive pre-stabilized via the
// integrator's useMemo.
export const TabStrip = memo(TabStripImpl);

function TabStripImpl({
  docs,
  activePath,
  sidebarCollapsed,
  onActivate,
  onRequestClose,
  onCloseAll,
  onCloseOthers,
  onCloseRight,
  onReorderTabs,
  onTogglePin,
  pinnedPaths,
  openLocalPaths,
  titleFor,
  isDirty,
  dirtyPaths,
  subscribeDirty,
  subscribeEdits,
  getEditSnapshot,
  computeTitle,
  overviewOpen,
  onOverviewOpenChange,
  onClosePaths,
}: TabStripProps) {
  const listRef = useRef<HTMLDivElement>(null);
  const overviewBtnRef = useRef<HTMLButtonElement>(null);
  const pointerDrag = useRef<PointerTabDrag | null>(null);
  const suppressClickPath = useRef<string | null>(null);
  // Path of the tab that last held DOM focus — lets a removed focused tab hand focus
  // to its replacement instead of stranding focus on <body>.
  const lastFocusedTab = useRef<string | null>(null);
  const [focusPath, setFocusPath] = useState<string | null>(null);
  const [draggingPath, setDraggingPath] = useState<string | null>(null);
  const [dropTarget, setDropTarget] = useState<TabDropTarget>(null);
  const [docMenu, setDocMenu] = useState<{ doc: LoadedDocument; x: number; y: number } | null>(
    null,
  );
  const mac = isMac();

  const tabElFor = useCallback((path: string): HTMLElement | null => {
    const list = listRef.current;
    if (!list) return null;
    // dataset compare — paths can hold quotes/backslashes that defeat attribute selectors.
    for (const el of list.querySelectorAll<HTMLElement>("[data-vr-tab]")) {
      if (el.dataset.vrTabPath === path) return el;
    }
    return null;
  }, []);

  // Roving tabindex: the tabbable tab is the last-focused one while it's still open,
  // else the active tab, else the first — so the strip is always one Tab stop.
  const rovingPath =
    focusPath && docs.some((d) => d.path === focusPath)
      ? focusPath
      : activePath && docs.some((d) => d.path === activePath)
        ? activePath
        : docs[0]?.path;

  // The selected tab owns the roving slot whenever activation happened off-strip
  // (Ctrl+Tab, palette, jumpToTab) — Tab into the strip lands on the selected tab.
  useEffect(() => {
    const focusInStrip = !!listRef.current?.contains(document.activeElement);
    setFocusPath((current) =>
      focusInStrip && current && docs.some((d) => d.path === current) ? current : activePath,
    );
  }, [activePath, docs]);

  // Focused tab closed → move focus to the replacement (the new active tab, else the
  // first) — but only when focus was stranded on <body>, never stealing it back.
  useEffect(() => {
    const focused = lastFocusedTab.current;
    if (!focused) return;
    if (docs.some((d) => d.path === focused)) return;
    lastFocusedTab.current = null;
    if (document.activeElement && document.activeElement !== document.body) return;
    const replacement =
      activePath && docs.some((d) => d.path === activePath) ? activePath : docs[0]?.path;
    if (replacement) tabElFor(replacement)?.focus();
  }, [docs, activePath, tabElFor]);

  // Keep the selected tab in view (activations via Ctrl+Tab / menu / palette too).
  // biome-ignore lint/correctness/useExhaustiveDependencies: docs is the trigger so a reordered active tab re-scrolls into view
  useEffect(() => {
    if (!activePath) return;
    const scroller = listRef.current;
    const reveal = () => revealTab(scroller, tabElFor(activePath));
    reveal();
    // Width changes (window resize, sidebar toggle, launch-time font/layout settle) can push
    // the active tab under the "+" without an activation — re-reveal on every resize.
    if (!scroller || typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(reveal);
    observer.observe(scroller);
    return () => observer.disconnect();
  }, [activePath, docs, tabElFor]);

  // ── Pointer drag-reorder (pointer events, NOT HTML5 DnD) ──────────────────
  // Same state machine as the sidebar's vertical list, hit-testing horizontally:
  // the insertion point is the first sibling tab whose left half contains clientX —
  // "before" it — else "after" the last tab.
  const dropTargetAt = useCallback((clientX: number, fromPath: string): TabDropTarget => {
    const list = listRef.current;
    if (!list) return null;
    const els = [...list.querySelectorAll<HTMLElement>("[data-vr-tab]")]
      .map((el) => ({ path: el.dataset.vrTabPath, box: el.getBoundingClientRect() }))
      .filter((x): x is { path: string; box: DOMRect } => Boolean(x.path) && x.path !== fromPath);
    for (const { path, box } of els) {
      if (clientX < box.left + box.width / 2) {
        return { toPath: path, position: "before" };
      }
    }
    const last = els[els.length - 1];
    return last ? { toPath: last.path, position: "after" } : null;
  }, []);

  const beginPointerTabDrag = useCallback((path: string, event: ReactPointerEvent) => {
    if (event.button !== 0) return;
    if ((event.target as HTMLElement | null)?.closest?.("[data-vr-tab-action]")) return;
    const element = event.currentTarget as HTMLElement;
    try {
      element.setPointerCapture?.(event.pointerId);
    } catch {
      /* jsdom / synthetic pointers may lack capture */
    }
    pointerDrag.current = {
      element,
      path,
      pointerId: event.pointerId,
      startX: event.clientX,
      startY: event.clientY,
      moved: false,
    };
  }, []);

  const consumeSuppressedClick = useCallback((path: string, event: ReactMouseEvent) => {
    if (suppressClickPath.current !== path) return false;
    event.preventDefault();
    event.stopPropagation();
    suppressClickPath.current = null;
    return true;
  }, []);

  useEffect(() => {
    const onMove = (event: PointerEvent) => {
      const drag = pointerDrag.current;
      if (!drag || event.pointerId !== drag.pointerId) return;
      if (
        !drag.moved &&
        Math.hypot(event.clientX - drag.startX, event.clientY - drag.startY) <= DRAG_THRESHOLD
      ) {
        return;
      }
      if (!drag.moved) {
        drag.moved = true;
        setDraggingPath(drag.path);
        document.body.style.userSelect = "none";
      }
      const target = dropTargetAt(event.clientX, drag.path);
      setDropTarget((current) =>
        current?.toPath === target?.toPath && current?.position === target?.position
          ? current
          : target,
      );
      event.preventDefault();
    };
    const onUp = (event: PointerEvent) => {
      const drag = pointerDrag.current;
      pointerDrag.current = null;
      if (!drag) return;
      try {
        drag.element.releasePointerCapture?.(drag.pointerId);
      } catch {
        /* jsdom */
      }
      document.body.style.userSelect = "";
      setDropTarget(null);
      setDraggingPath(null);
      if (!drag.moved) return;
      event.preventDefault();
      // Only swallow the click when the drag lands on a REAL reorder target — a jitter
      // past the threshold with no target should still let the tab ACTIVATE (mirrors
      // Sidebar's fix for the "tab activation is laggy" report). The horizontal model
      // always resolves SOME before/after target, so a drop that leaves the tab where it
      // already sits (before its current right neighbor / after its current left one)
      // counts as a no-op too — no reorder call, no click suppression.
      const target = dropTargetAt(event.clientX, drag.path);
      if (target) {
        const fromIndex = docs.findIndex((d) => d.path === drag.path);
        const toIndex = docs.findIndex((d) => d.path === target.toPath);
        const isNoOp =
          (target.position === "before" && toIndex === fromIndex + 1) ||
          (target.position === "after" && toIndex === fromIndex - 1);
        if (!isNoOp) {
          suppressClickPath.current = drag.path;
          window.setTimeout(() => {
            if (suppressClickPath.current === drag.path) suppressClickPath.current = null;
          }, 0);
          onReorderTabs(drag.path, target.toPath, target.position);
        }
      }
    };
    const onCancel = (event: PointerEvent) => {
      const drag = pointerDrag.current;
      if (!drag || event.pointerId !== drag.pointerId) return;
      pointerDrag.current = null;
      try {
        drag.element.releasePointerCapture?.(drag.pointerId);
      } catch {
        /* jsdom */
      }
      document.body.style.userSelect = "";
      setDropTarget(null);
      setDraggingPath(null);
    };
    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", onUp);
    window.addEventListener("pointercancel", onCancel);
    return () => {
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", onUp);
      window.removeEventListener("pointercancel", onCancel);
      document.body.style.userSelect = "";
    };
  }, [docs, dropTargetAt, onReorderTabs]);

  // Vertical wheel/trackpad delta scrolls the strip horizontally (trackpads also send
  // deltaX — both fold into scrollLeft). No preventDefault: React's wheel listener is
  // passive and the header never scrolls vertically anyway.
  const onWheel = useCallback((event: ReactWheelEvent<HTMLDivElement>) => {
    const el = listRef.current;
    if (!el) return;
    const unit = event.deltaMode === 1 ? 16 : 1; // DOM_DELTA_LINE → ~16px per line
    const delta = (event.deltaY + event.deltaX) * unit;
    if (delta) el.scrollLeft += delta;
  }, []);

  const onScroll = useCallback(() => syncScrolledEdge(listRef.current), []);

  // ── Keyboard: roving tabindex + automatic activation (ARIA tabs pattern) ──
  const onListKeyDown = useCallback(
    (event: ReactKeyboardEvent<HTMLDivElement>) => {
      const tabEl = (event.target as HTMLElement | null)?.closest?.(
        "[data-vr-tab]",
      ) as HTMLElement | null;
      if (!tabEl) return;
      const path = tabEl.dataset.vrTabPath;
      if (!path) return;
      const index = docs.findIndex((d) => d.path === path);
      if (index < 0) return;
      let nextIndex: number | null = null;
      switch (event.key) {
        case "ArrowRight":
          nextIndex = (index + 1) % docs.length;
          break;
        case "ArrowLeft":
          nextIndex = (index - 1 + docs.length) % docs.length;
          break;
        case "Home":
          nextIndex = 0;
          break;
        case "End":
          nextIndex = docs.length - 1;
          break;
        case "Delete":
        case "Backspace":
          event.preventDefault();
          onRequestClose(path);
          return;
        case "Enter":
        case " ":
          // A real <button> inside the tab (the ×) handles its own Enter/Space — don't
          // double-fire activate + close for the same keypress.
          if ((event.target as HTMLElement).tagName === "BUTTON") return;
          event.preventDefault();
          onActivate(path);
          return;
        default:
          return;
      }
      event.preventDefault();
      const next = docs[nextIndex];
      if (!next) return;
      // Automatic activation: focus moves and the tab is selected in the same step.
      tabElFor(next.path)?.focus();
      onActivate(next.path);
    },
    [docs, onActivate, onRequestClose, tabElFor],
  );

  const onTabFocused = useCallback((path: string) => {
    lastFocusedTab.current = path;
    setFocusPath(path);
  }, []);

  const onTabBlurred = useCallback((relatedTarget: EventTarget | null) => {
    const node = relatedTarget as Node | null;
    if (!node) return; // focus went nowhere/nowhere visible — the docs-change effect decides
    if (listRef.current?.contains(node)) return; // moved to a sibling tab / the × inside it
    lastFocusedTab.current = null;
  }, []);

  const openDocMenu = useCallback(
    (doc: LoadedDocument, event: ReactMouseEvent) => {
      event.preventDefault();
      onOverviewOpenChange(false);
      setDocMenu({ doc, x: event.clientX, y: event.clientY });
    },
    [onOverviewOpenChange],
  );

  // Bound to the menu's doc so the same menu instance works for any tab.
  const closeMenuOthers = useCallback(() => {
    if (docMenu) onCloseOthers(docMenu.doc.path);
  }, [docMenu, onCloseOthers]);
  const closeMenuRight = useCallback(() => {
    if (docMenu) onCloseRight(docMenu.doc.path);
  }, [docMenu, onCloseRight]);

  return (
    <div className="vr-tabstrip" data-tauri-drag-region>
      {/* With the sidebar collapsed its rail is narrower than the macOS traffic lights; this
          spacer keeps the strip clear of them. Sidebar actions live in the rail, not here. */}
      {sidebarCollapsed && mac && (
        <div className="vr-tabstrip-lead-lights" data-tauri-drag-region />
      )}
      {/* Tab overview sits at the strip's start, beside the sidebar control, never at the far
          right where it crowded the tabs. */}
      <div className="vr-tabstrip-ov" data-tauri-drag-region>
        <button
          ref={overviewBtnRef}
          type="button"
          className="vr-ov-btn vr-press vr-focus"
          onClick={() => onOverviewOpenChange(!overviewOpen)}
          title={`Show all tabs (${shortcutLabel("mod+shift+a")})`}
          aria-label="Show all tabs"
          aria-haspopup="dialog"
          aria-expanded={overviewOpen}
        >
          <Cards size={15} weight="regular" />
          {docs.length > 3 && <span className="vr-ov-count">{docs.length}</span>}
        </button>
      </div>
      <button
        type="button"
        className="vr-tab-arrow vr-tab-arrow-start vr-press vr-focus"
        onClick={() => scrollStrip(listRef.current, -1)}
        title="Scroll tabs left"
        aria-label="Scroll tabs left"
        tabIndex={-1}
      >
        <CaretLeft size={13} weight="bold" />
      </button>
      {/* Horizontal scroller (drag region in its gaps). It shrinks to its tabs, so the "+"
          after it follows the last tab and, once tabs overflow, sits just past the strip's
          faded end — tabs never slide underneath it. */}
      <div
        ref={listRef}
        className="vr-tabs"
        data-tauri-drag-region
        onWheel={onWheel}
        onScroll={onScroll}
        onKeyDown={onListKeyDown}
      >
        <div
          role="tablist"
          aria-label="Open documents"
          aria-orientation="horizontal"
          className="vr-tabs-list"
        >
          {docs.map((doc) => (
            <Tab
              key={doc.tabKey ?? doc.path}
              doc={doc}
              title={titleFor(doc)}
              titleNode={
                isUntitled(doc.path) ? (
                  <LiveTitle
                    doc={doc}
                    compute={computeTitle}
                    subscribeEdits={subscribeEdits}
                    getEditSnapshot={getEditSnapshot}
                  />
                ) : undefined
              }
              isDirty={isDirty}
              dirtyHint={dirtyPaths.has(doc.path)}
              subscribeDirty={subscribeDirty}
              active={doc.path === activePath}
              tabbable={doc.path === rovingPath}
              dragging={draggingPath === doc.path}
              dropPosition={dropTarget?.toPath === doc.path ? dropTarget.position : undefined}
              onActivate={onActivate}
              onRequestClose={onRequestClose}
              onPointerDragStart={beginPointerTabDrag}
              onConsumeSuppressedClick={consumeSuppressedClick}
              onContextMenu={openDocMenu}
              onFocused={onTabFocused}
              onBlurred={onTabBlurred}
            />
          ))}
        </div>
      </div>
      <button
        type="button"
        className="vr-tab-arrow vr-tab-arrow-end vr-press vr-focus"
        onClick={() => scrollStrip(listRef.current, 1)}
        title="Scroll tabs right"
        aria-label="Scroll tabs right"
        tabIndex={-1}
      >
        <CaretRight size={13} weight="bold" />
      </button>
      <div className="vr-tabstrip-fill" data-tauri-drag-region />
      {overviewOpen && (
        <TabOverview
          docs={docs}
          activePath={activePath}
          titleFor={titleFor}
          isDirty={isDirty}
          dirtyPaths={dirtyPaths}
          subscribeDirty={subscribeDirty}
          subscribeEdits={subscribeEdits}
          getEditSnapshot={getEditSnapshot}
          computeTitle={computeTitle}
          onActivate={onActivate}
          onClosePaths={onClosePaths}
          onClose={() => onOverviewOpenChange(false)}
          returnFocusRef={overviewBtnRef}
        />
      )}
      {docMenu && (
        <DocumentMenu
          menu={docMenu}
          openLocalPaths={openLocalPaths}
          pinned={pinnedPaths.has(docMenu.doc.path)}
          onClose={() => setDocMenu(null)}
          onActivate={onActivate}
          onCloseDoc={onRequestClose}
          onCloseAll={onCloseAll}
          onTogglePin={onTogglePin}
          titleFor={titleFor}
          onCloseOthers={closeMenuOthers}
          onCloseRight={closeMenuRight}
        />
      )}
    </div>
  );
}

const Tab = memo(TabImpl);

// One tab: format icon tile + name (masked fade, no ellipsis) + dirty dot + close ×.
// Document-keyed id (tabDomId) pairs with the document pane's aria-labelledby, so the pane's
// label names the selected tab even when tabs reorder.
function TabImpl({
  doc,
  title,
  titleNode,
  isDirty,
  dirtyHint,
  subscribeDirty,
  active,
  tabbable,
  dragging,
  dropPosition,
  onActivate,
  onRequestClose,
  onPointerDragStart,
  onConsumeSuppressedClick,
  onContextMenu,
  onFocused,
  onBlurred,
}: {
  doc: LoadedDocument;
  title: string;
  titleNode?: ReactNode;
  isDirty: (doc: LoadedDocument) => boolean;
  dirtyHint: boolean;
  subscribeDirty: (path: string, cb: () => void) => () => void;
  active: boolean;
  tabbable: boolean;
  dragging: boolean;
  dropPosition?: "before" | "after";
  onActivate: (path: string) => void;
  onRequestClose: (path: string) => void;
  onPointerDragStart: (path: string, event: ReactPointerEvent) => void;
  onConsumeSuppressedClick: (path: string, event: ReactMouseEvent) => boolean;
  onContextMenu: (doc: LoadedDocument, event: ReactMouseEvent) => void;
  onFocused: (path: string) => void;
  onBlurred: (relatedTarget: EventTarget | null) => void;
}) {
  const dirty = useTabDirty(doc, dirtyHint, isDirty, subscribeDirty);
  const accessibleTitle = `${title}${dirty ? " — Unsaved edits" : ""}`;
  const tooltip = doc.format === "web" ? (doc.web?.url ?? doc.path) : doc.path;
  return (
    <div
      role="tab"
      id={tabDomId(doc)}
      aria-selected={active}
      aria-controls="vr-doc-pane"
      aria-label={accessibleTitle}
      tabIndex={tabbable ? 0 : -1}
      title={tooltip}
      data-vr-tab
      data-vr-tab-path={doc.path}
      data-vr-tab-active={active || undefined}
      className={`vr-tab${active ? " vr-tab-active" : ""}${dirty ? " vr-tab-dirty" : ""}${dragging ? " vr-tab-dragging" : ""}`}
      onPointerDown={(event) => onPointerDragStart(doc.path, event)}
      onClick={(event) => {
        if ((event.target as HTMLElement | null)?.closest?.("[data-vr-tab-action]")) {
          return;
        }
        if (onConsumeSuppressedClick(doc.path, event)) return;
        onActivate(doc.path);
      }}
      onAuxClick={(event) => {
        // Middle-click closes — anywhere on the tab, including over the ×.
        if (event.button !== 1) return;
        event.preventDefault();
        onRequestClose(doc.path);
      }}
      onContextMenu={(event) => onContextMenu(doc, event)}
      onFocus={() => onFocused(doc.path)}
      onBlur={(event) => onBlurred(event.relatedTarget)}
    >
      {active && <TabFuseCurves size={12} />}
      <span className="vr-tab-ficon" aria-hidden="true">
        <FileTypeIcon format={doc.format} path={doc.path} size={16} />
      </span>
      <span className="vr-tab-name">{titleNode ?? title}</span>
      <span className="vr-tab-dot" data-vr-tab-dirty-indicator={dirty || undefined} />
      <button
        type="button"
        data-vr-tab-action
        className="vr-tab-x vr-press vr-focus"
        aria-label={`Close ${title}`}
        title="Close"
        onClick={(event) => {
          event.stopPropagation();
          onRequestClose(doc.path);
        }}
      >
        <X size={12} weight="bold" />
      </button>
      {dropPosition && (
        <span className="vr-tab-dropbar" data-pos={dropPosition} aria-hidden="true" />
      )}
    </div>
  );
}

/** Marks which ends of the scroller have tabs hidden past them, so CSS fades those edges
 *  instead of slicing a tab. Set directly on the element: scrolling must not re-render. */
function syncScrolledEdge(el: HTMLElement | null) {
  if (!el) return;
  if (el.scrollLeft > 2) el.dataset.vrScrolled = "";
  else delete el.dataset.vrScrolled;
  if (el.scrollLeft + el.clientWidth < el.scrollWidth - 2) el.dataset.vrMoreEnd = "";
  else delete el.dataset.vrMoreEnd;
}

/** Scroll only the strip (never ancestors, unlike scrollIntoView) so `tab` sits clear of
 *  both edge fades. */
const REVEAL_PAD_START = 28;
const REVEAL_PAD_END = 28;
function revealTab(scroller: HTMLElement | null, tab: HTMLElement | null | undefined) {
  if (!scroller || !tab) return;
  const s = scroller.getBoundingClientRect();
  const t = tab.getBoundingClientRect();
  if (t.left < s.left + REVEAL_PAD_START) scroller.scrollLeft -= s.left + REVEAL_PAD_START - t.left;
  else if (t.right > s.right - REVEAL_PAD_END)
    scroller.scrollLeft += t.right - (s.right - REVEAL_PAD_END);
  syncScrolledEdge(scroller);
}

/** Page the strip by most of its visible width (the ‹ › buttons shown while tabs overflow). */
function scrollStrip(el: HTMLElement | null, direction: -1 | 1) {
  if (!el) return;
  el.scrollBy({ left: direction * Math.max(120, el.clientWidth * 0.7), behavior: "smooth" });
}
