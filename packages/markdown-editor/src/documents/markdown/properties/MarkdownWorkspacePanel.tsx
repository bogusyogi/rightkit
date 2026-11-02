/**
 * Reachable Markdown authoring surfaces backed by one document core.
 *
 * This intentionally composes existing panels instead of recreating their
 * mutation paths: every change continues through the revisioned core bus.
 */

import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
  type RefObject,
  type UIEventHandler,
} from "react";
import { useDismiss } from "@rightkit/platform-ui/headless";
import { createShortcutRegistry } from "@rightkit/react-shell";
import { setBlockType } from "prosemirror-commands";
import { TextSelection } from "prosemirror-state";
import { useOptionalBlockDrafts } from "@rightkit/document-core/app/BlockDraftContext";
import { Dropdown, Group, Item, Sep, TBtn } from "@rightkit/document-core/documents/toolbarPrimitives";
import { DocumentToolbarShell } from "@rightkit/document-core/documents/DocumentToolbarShell";
import { MARKDOWN_FORMAT_EVENT } from "@rightkit/document-core/app/shell/nativeMenu";
import { MarkdownSelectionBubble } from "./MarkdownSelectionBubble.js";
import { insertTableTemplate } from "../markdownTools.js";
import { ListNumbers } from "@phosphor-icons/react/dist/icons/ListNumbers";
import { CheckSquare } from "@phosphor-icons/react/dist/icons/CheckSquare";
import { Quotes } from "@phosphor-icons/react/dist/icons/Quotes";
import { ArrowUp } from "@phosphor-icons/react/dist/icons/ArrowUp";
import { ArrowDown } from "@phosphor-icons/react/dist/icons/ArrowDown";
import { TextB } from "@phosphor-icons/react/dist/icons/TextB";
import { TextItalic } from "@phosphor-icons/react/dist/icons/TextItalic";
import { TextStrikethrough } from "@phosphor-icons/react/dist/icons/TextStrikethrough";
import { Code } from "@phosphor-icons/react/dist/icons/Code";
import { ListBullets } from "@phosphor-icons/react/dist/icons/ListBullets";
import { LinkSimple } from "@phosphor-icons/react/dist/icons/LinkSimple";
import { SlidersHorizontal } from "@phosphor-icons/react/dist/icons/SlidersHorizontal";
import { TreeStructure } from "@phosphor-icons/react/dist/icons/TreeStructure";
import { WarningCircle } from "@phosphor-icons/react/dist/icons/WarningCircle";
import { X } from "@phosphor-icons/react/dist/icons/X";
import { Highlighter } from "@phosphor-icons/react/dist/icons/Highlighter";
import { ImageSquare } from "@phosphor-icons/react/dist/icons/ImageSquare";
import { Plus } from "@phosphor-icons/react/dist/icons/Plus";
import type { Icon } from "@phosphor-icons/react";
import { MarkdownIssuesPanel } from "../analysis/MarkdownIssuesPanel.js";
import { useMarkdownAnalysis } from "../analysis/useMarkdownAnalysis.js";
import { MarkdownInsertMenu } from "../insert/MarkdownInsertMenu.js";
import type { MarkdownDocumentCore } from "../core/MarkdownDocumentCore.js";
import {
  canMoveMarkdownBlock,
  computeMoveMarkdownBlock,
  topLevelMarkdownBlockIndexAt,
  type MoveMarkdownBlockDirection,
} from "../core/moveMarkdownBlock.js";
import { MarkdownPropertiesPanel } from "./MarkdownPropertiesPanel.js";
import { useMarkdownOutline } from "./useMarkdownOutline.js";
import type { OutlineNode } from "./markdownOutline.js";
import { MarkdownLinkPanel } from "../references/MarkdownLinkPanel.js";
import { acquireMarkdownResource } from "../markdownResourceLifecycle.js";
import { MarkdownTableToolbarGroup } from "../tables/MarkdownTableControls.js";
import type { MarkdownTableContext } from "../tables/markdownTableCommands.js";
import type { MarkdownToolbarState } from "../MarkdownEditor.js";
import { DocumentToolbarSearch, type DocumentSearchMode } from "@rightkit/document-core/documents/DocumentToolbarFind";
import { BackToTopButton } from "@rightkit/document-core/documents/BackToTopButton";
import { scrollBehaviorForMotion } from "@rightkit/document-core/state/motion";
import { shortcutLabel } from "@rightkit/document-core/platform";
import { DocBar } from "@rightkit/document-core/app/shell/docBar";

type WorkspaceSurface = "issues" | "properties" | "outline" | "insert" | "links";
type MarkdownSelectionRange = { anchor: number; head: number };
// Read mode keeps only the viewing surfaces; insert/links/properties mutate the document.
const READ_SURFACES: readonly WorkspaceSurface[] = ["issues", "outline"];

export function MarkdownWorkspacePanel({
  core,
  onJumpToRange,
  getInsertAt,
  caretOffset,
  documentPath,
  safeHtml,
  chromeHidden = false,
  readOnly = false,
  children,
  captureSelection,
  selection,
  onInsertImage,
  getEditorView,
  tableContext,
  toolbarState,
  scrollContainerRef,
  onScroll,
  findBar,
  findMode,
  onRequestFind,
  onRequestReplace,
}: {
  core: MarkdownDocumentCore;
  onJumpToRange: (range: { from: number; to: number }) => void;
  getInsertAt: () => number;
  caretOffset: number;
  documentPath?: string;
  safeHtml: boolean;
  /** Hide the toolbar + inspector while keeping children mounted in the SAME tree position —
   *  the source drawer sets this instead of unmounting the panel, so the hybrid editor never
   *  remounts on a preview/source toggle. `surface` survives, restoring the inspector on return. */
  chromeHidden?: boolean;
  /** Wave 2 Read mode: the floating format bar disappears, the source drawer stays closed, and
   *  only the viewing surfaces (issues/outline) remain reachable. */
  readOnly?: boolean;
  /** Wave 2: "Source" toggle at the end of the Edit format bar — opens the source drawer
   *  (a ViewMode transition, not a Read/Edit one). */
  onToggleSource?: () => void;
  children: ReactNode;
  captureSelection?: () => MarkdownSelectionRange | undefined;
  selection?: MarkdownSelectionRange;
  onInsertImage?: () => void;
  getEditorView?: () => import("prosemirror-view").EditorView | null;
  /** Current caret table context (null outside a table); drives the toolbar's contextual
   *  whole-table/Row/Column/Align/More group. Owned by the split pane, bubbled up from the editor's
   *  own ProseMirror transactions — see MarkdownEditor's onTableContextChange. */
  tableContext?: MarkdownTableContext | null;
  toolbarState?: MarkdownToolbarState;
  scrollContainerRef?: RefObject<HTMLDivElement | null>;
  onScroll?: UIEventHandler<HTMLDivElement>;
  findBar?: ReactNode;
  findMode?: DocumentSearchMode;
  onRequestFind?: () => void;
  onRequestReplace?: () => void;
}): ReactNode {
  const effectiveToolbarState = toolbarState ?? {
    headingLevel: null,
    bold: false,
    italic: false,
    strike: false,
    code: false,
    highlight: false,
  };
  const [surface, setSurface] = useState<WorkspaceSurface | null>(null);
  const activeSurface =
    chromeHidden || (readOnly && surface !== null && !READ_SURFACES.includes(surface))
      ? null
      : surface;
  const workspaceRef = useRef<HTMLDivElement | null>(null);
  const [workspaceHeight, setWorkspaceHeight] = useState(0);
  useEffect(() => {
    const el = workspaceRef.current;
    if (!el) return;
    const measure = () => setWorkspaceHeight(el.getBoundingClientRect().height);
    measure();
    if (typeof ResizeObserver === "undefined") {
      window.addEventListener("resize", measure);
      return () => window.removeEventListener("resize", measure);
    }
    const observer = new ResizeObserver(measure);
    const release = acquireMarkdownResource("observer");
    observer.observe(el);
    return () => {
      observer.disconnect();
      release();
    };
  }, []);
  const toolbarRef = useRef<HTMLDivElement | null>(null);
  const [toolbarHeight, setToolbarHeight] = useState(0);
  // biome-ignore lint/correctness/useExhaustiveDependencies: activeSurface triggers re-measure when surface changes while chrome visible
  useEffect(() => {
    const el = toolbarRef.current;
    if (!el || chromeHidden) {
      setToolbarHeight(0);
      return;
    }
    const measure = () => setToolbarHeight(el.getBoundingClientRect().height);
    measure();
    if (typeof ResizeObserver === "undefined") {
      window.addEventListener("resize", measure);
      return () => window.removeEventListener("resize", measure);
    }
    const ro = new ResizeObserver(measure);
    const release = acquireMarkdownResource("observer");
    ro.observe(el);
    return () => {
      ro.disconnect();
      release();
    };
  }, [chromeHidden, activeSurface]);
  const { analysis } = useMarkdownAnalysis({
    core: activeSurface === "issues" ? core : undefined,
    documentPath,
    safeHtml,
  });
  const { nodes, activeNodeId } = useMarkdownOutline({
    core: activeSurface === "outline" ? core : undefined,
    caretOffset,
  });
  const toolButtonRefs = useRef<Partial<Record<WorkspaceSurface, HTMLButtonElement | null>>>({});
  const selectSurface = (next: WorkspaceSurface): void => {
    if (readOnly && !READ_SURFACES.includes(next)) return;
    setSurface((current) => (current === next ? null : next));
  };
  const closeSurface = (): void => {
    const trigger = surface ? toolButtonRefs.current[surface] : null;
    setSurface(null);
    trigger?.focus();
  };
  // "Latest ref" so the effect below can stay mounted/unmounted purely on
  // whether a panel is open, without resubscribing on every render.
  const closeSurfaceRef = useRef(closeSurface);
  closeSurfaceRef.current = closeSurface;

  // Escape closes the open inspector and returns focus to the tool button that
  // opened it. The listener is only attached while a panel is open — when closed,
  // it must not exist at all, so Escape keeps reaching the FindBar and modal
  // dialogs that also own that key.
  // Bubble phase on the document, like the handler it replaces.
  const documentTarget = useRef<EventTarget | null>(typeof document === "undefined" ? null : document);
  useDismiss(() => closeSurfaceRef.current(), {
    enabled: Boolean(activeSurface),
    escapeTarget: documentTarget,
    outside: false,
    preventDefault: true,
    stopPropagation: false,
    imeGuard: false,
  });
  useEffect(() => {
    if (activeSurface === "links" || readOnly) return;
    // Ctrl/Cmd+K through a registry scoped to this panel (bubble phase on the window). Ctrl, Meta or
    // both, with no Alt or Shift, and an already-prevented event is skipped: what the raw listener did. A focused input, textarea or select keeps the key: `run` answers `false`, which
    // reports "not handled" without preventing the default.
    const registry = createShortcutRegistry();
    registry.register({
      id: "markdown-workspace.open-links",
      label: "Open links panel",
      chord: ["ctrl+k", "meta+k", "ctrl+meta+k"],
      hidden: true,
      allowComposing: true,
      run: (event) => {
        const target = event.target as HTMLElement | null;
        if (target instanceof HTMLElement && target.matches("input,textarea,select")) return false;
        captureSelection?.();
        setSurface("links");
      },
    });
    return registry.attach(window);
  }, [activeSurface, captureSelection, readOnly]);

  // Edit adds the floating format bar under the toolbar (8px gap + 42px capsule): push the
  // inspector below it so the capsule never covers the panel header.
  const inspectorTop =
    chromeHidden || toolbarHeight === 0 ? 64 : toolbarHeight + (readOnly ? 24 : 62);
  const blockDrafts = useOptionalBlockDrafts();
  const [draftTick, setDraftTick] = useState(0);
  useEffect(() => {
    if (!blockDrafts) return;
    return blockDrafts.subscribe(() => setDraftTick((t) => t + 1));
  }, [blockDrafts]);
  // Access draftTick to force memo invalidation on registry changes without
  // adding an extra render-triggering state to the activeDraft memo itself.
  void draftTick;
  // biome-ignore lint/correctness/useExhaustiveDependencies: draftTick forces invalidation on registry changes
  const activeDraft = useMemo(() => {
    if (!blockDrafts) return null;
    const docId = documentPath ?? core.state.documentId;
    return blockDrafts.get({ documentPath: docId, pagePath: docId });
  }, [blockDrafts, documentPath, core, draftTick]);
  const getDraftIdentity = () => {
    const docId = documentPath ?? core.state.documentId;
    return { documentPath: docId, pagePath: docId } as const;
  };
  const getInlineWrapDisabledReason = (captured?: MarkdownSelectionRange): string | null => {
    if (!activeDraft) return null;
    if (activeDraft.status === "stale")
      return activeDraft.reason ?? "Block changed outside this editor.";
    if (activeDraft.status === "committing") return "Committing…";
    const sel = captured ?? selection ?? { anchor: getInsertAt(), head: getInsertAt() };
    const from = Math.min(sel.anchor, sel.head);
    const to = Math.max(sel.anchor, sel.head);
    const base = activeDraft.sourceRange?.start ?? 0;
    const end = activeDraft.sourceRange?.end ?? base + activeDraft.originalText.length;
    if (from < base || to > end) return "No valid target in edited block";
    return null;
  };
  const getLinePrefixDisabledReason = (): string | null => {
    if (!activeDraft) return null;
    if (activeDraft.status === "stale")
      return activeDraft.reason ?? "Block changed outside this editor.";
    if (activeDraft.status === "committing") return "Committing…";
    const at = getInsertAt();
    const base = activeDraft.sourceRange?.start ?? 0;
    const end = activeDraft.sourceRange?.end ?? base + activeDraft.originalText.length;
    if (at < base || at > end) return "No valid target in edited block";
    return null;
  };
  const getBlockInsertDisabledReason = (): string | null => {
    if (!activeDraft) return null;
    if (activeDraft.status === "stale")
      return activeDraft.reason ?? "Block changed outside this editor.";
    if (activeDraft.status === "committing") return "Committing…";
    const at = getInsertAt();
    const base = activeDraft.sourceRange?.start ?? 0;
    const end = activeDraft.sourceRange?.end ?? base + activeDraft.originalText.length;
    if (at < base || at > end) return "No valid target in edited block";
    return null;
  };
  const applyInlineWrap = (before: string, after: string, placeholder = "text") => {
    const captured = captureSelection?.();
    const disabled = getInlineWrapDisabledReason(captured);
    if (disabled) return;
    if (activeDraft && blockDrafts) {
      const sel = captured ?? selection ?? { anchor: getInsertAt(), head: getInsertAt() };
      const from = Math.min(sel.anchor, sel.head);
      const to = Math.max(sel.anchor, sel.head);
      const base = activeDraft.sourceRange?.start ?? 0;
      const localFrom = Math.max(0, Math.min(from - base, activeDraft.currentText.length));
      const localTo = Math.max(localFrom, Math.min(to - base, activeDraft.currentText.length));
      const current = activeDraft.currentText;
      const selected = current.slice(localFrom, localTo) || placeholder;
      const insert = `${before}${selected}${after}`;
      const next = current.slice(0, localFrom) + insert + current.slice(localTo);
      const bookmark = activeDraft.selectionBookmark;
      blockDrafts.update(getDraftIdentity(), next, bookmark);
      getEditorView?.()?.focus();
      return;
    }
    const sel = captured ?? selection ?? { anchor: getInsertAt(), head: getInsertAt() };
    let from = Math.min(sel.anchor, sel.head);
    let to = Math.max(sel.anchor, sel.head);
    const src = core.snapshot().logicalSource;
    // A selection that ends at the edge of styled text maps to a source range that can include the
    // closing (or opening) emphasis delimiters, e.g. `text***` for selected "text" in ***text***.
    // Wrapping that range would interleave the new delimiters with the existing ones, so trim edge
    // delimiters the rendered selection does not contain.
    const view = getEditorView?.();
    const rendered =
      view && !view.state.selection.empty
        ? view.state.doc.textBetween(view.state.selection.from, view.state.selection.to)
        : "";
    const isDelimiter = (char: string | undefined) => !!char && "*_~=`".includes(char);
    while (to > from && isDelimiter(src[to - 1]) && !rendered.endsWith(src[to - 1])) to -= 1;
    while (from < to && isDelimiter(src[from]) && !rendered.startsWith(src[from])) from += 1;
    const selected = src.slice(from, to) || placeholder;
    const insert = `${before}${selected}${after}`;
    const selectionStart = from + before.length;
    const selectionEnd = selectionStart + selected.length;
    const backward = sel.anchor > sel.head;
    // Commit hints refer to the new source, including inserted delimiters.
    core.postSelectionHint({
      anchor: backward ? selectionEnd : selectionStart,
      head: backward ? selectionStart : selectionEnd,
      assoc: backward ? -1 : 1,
      surface: "prosemirror",
      nodeId: null,
    });
    core.applyCommand({
      edits: [{ from, to, insert, inheritMode: "preserve" }],
      origin: "block-editor",
      label: "Format",
      isolateHistory: true,
    });
    getEditorView?.()?.focus();
  };
  const applyLinePrefix = (prefix: string) => {
    captureSelection?.();
    const disabled = getLinePrefixDisabledReason();
    if (disabled) return;
    if (activeDraft && blockDrafts) {
      const at = getInsertAt();
      const base = activeDraft.sourceRange?.start ?? 0;
      const current = activeDraft.currentText;
      const localAt = Math.max(0, Math.min(at - base, current.length));
      const lineStart = current.lastIndexOf("\n", localAt - 1) + 1;
      const next = current.slice(0, lineStart) + prefix + current.slice(lineStart);
      const bookmark = activeDraft.selectionBookmark;
      blockDrafts.update(getDraftIdentity(), next, bookmark);
      getEditorView?.()?.focus();
      return;
    }
    const at = getInsertAt();
    const src = core.snapshot().logicalSource;
    const lineStart = src.lastIndexOf("\n", at - 1) + 1;
    core.applyCommand({
      edits: [{ from: lineStart, to: lineStart, insert: prefix, inheritMode: "preserve" }],
      origin: "block-editor",
      label: "Format",
      isolateHistory: true,
    });
    getEditorView?.()?.focus();
  };
  // Paragraph-style command: 0 = body paragraph, 1–6 = heading levels. Shared DOM-selection
  // reconcile before changing block type (toolbar mousedown preserves the DOM range).
  const applyBlockStyle = (level: 0 | 1 | 2 | 3 | 4 | 5 | 6) => {
    captureSelection?.();
    if (activeDraft) return;
    const view = getEditorView?.();
    const nodeType =
      level === 0 ? view?.state.schema.nodes.paragraph : view?.state.schema.nodes.heading;
    if (!view || !nodeType) return;
    // Browser DOM selection can advance one pointer task before ProseMirror's selection state.
    // Toolbar mousedown preserves that DOM range, so reconcile it before changing block type.
    const domSelection = window.getSelection();
    const anchorNode = domSelection?.anchorNode;
    if (anchorNode && view.dom.contains(anchorNode)) {
      try {
        const position = view.posAtDOM(anchorNode, domSelection?.anchorOffset ?? 0);
        if (position >= 0 && position <= view.state.doc.content.size) {
          const selection = TextSelection.near(view.state.doc.resolve(position));
          if (!selection.eq(view.state.selection))
            view.dispatch(view.state.tr.setSelection(selection));
        }
      } catch {
        // A detached DOM range falls back to ProseMirror's last owned selection.
      }
    }
    const current = view.state.selection.$from.parent;
    const attrs = {
      ...current.attrs,
      ...(level === 0 ? {} : { level }),
      rawSource: "",
      rawText: current.textContent,
    };
    if (setBlockType(nodeType, attrs)(view.state, view.dispatch, view)) view.focus();
  };
  // Move the top-level block containing the caret up/down. Self-contained: derives the
  // block index from `core.snapshot()` + the `caretOffset` prop this panel already
  // receives (the same inputs the deleted floating block-controls panel used), and
  // dispatches the identical `moveMarkdownBlock` edit the ⌥↑/⌥↓ keymap in
  // MarkdownEditor.tsx uses — same command, same one-step undo, same selection-hint
  // restore, just triggered from the toolbar instead of a keybinding.
  const moveBlock = (direction: MoveMarkdownBlockDirection) => {
    captureSelection?.();
    const snapshot = core.snapshot();
    const blockIndex = topLevelMarkdownBlockIndexAt(snapshot, caretOffset);
    if (blockIndex === null) return;
    const move = computeMoveMarkdownBlock(snapshot, blockIndex, direction);
    if (!move) return;
    const newOffset = move.mapOffset(caretOffset);
    core.postSelectionHint({
      surface: "prosemirror",
      nodeId: null,
      anchor: newOffset,
      head: newOffset,
      assoc: 1,
    });
    core.applyCommand({
      edits: [move.edit],
      expectedRevision: snapshot.revision,
      origin: "hybrid",
      label: "BlockEdit",
      isolateHistory: true,
      timestampMs: Math.round(performance.now()),
    });
    getEditorView?.()?.focus();
  };
  const applyBlockInsert = (insert: string) => {
    captureSelection?.();
    const disabled = getBlockInsertDisabledReason();
    if (disabled) return;
    if (activeDraft && blockDrafts) {
      const at = getInsertAt();
      const base = activeDraft.sourceRange?.start ?? 0;
      const current = activeDraft.currentText;
      const localAt = Math.max(0, Math.min(at - base, current.length));
      const next = current.slice(0, localAt) + insert + current.slice(localAt);
      const bookmark = activeDraft.selectionBookmark;
      blockDrafts.update(getDraftIdentity(), next, bookmark);
      getEditorView?.()?.focus();
      return;
    }
    const at = getInsertAt();
    core.applyCommand({
      edits: [{ from: at, to: at, insert, inheritMode: "preserve" }],
      origin: "block-editor",
      label: "BlockEdit",
      isolateHistory: true,
    });
    getEditorView?.()?.focus();
  };
  // Unlike the HTML/MDX back-to-top pills (always rendered), this one behaves like a web page:
  // hidden until you are a screenful down. `onScroll` fires every frame while scrolling, so only
  // commit state when the boolean actually flips — otherwise a long document re-renders the whole
  // editor on every scroll tick.
  const [showBackToTop, setShowBackToTop] = useState(false);
  const handleScroll = useCallback<UIEventHandler<HTMLDivElement>>(
    (event) => {
      const el = event.currentTarget;
      const past = el.scrollTop > el.clientHeight;
      setShowBackToTop((prev) => (prev === past ? prev : past));
      onScroll?.(event);
    },
    [onScroll],
  );
  const scrollToTop = useCallback(() => {
    scrollContainerRef?.current?.scrollTo({ top: 0, behavior: scrollBehaviorForMotion() });
  }, [scrollContainerRef]);

  // macOS Format/Insert menu items route here (see documentMenuCommands) so the menu bar runs the
  // same commands as the floating format bar. Only while editing.
  const menuHandlers = useRef<Record<string, () => void>>({});
  menuHandlers.current = {
    bold: () => applyInlineWrap("**", "**"),
    italic: () => applyInlineWrap("*", "*"),
    strikethrough: () => applyInlineWrap("~~", "~~"),
    "insert-link": () => applyInlineWrap("[", "](https://)", "text"),
    code: () => applyInlineWrap("`", "`", "code"),
    highlight: () => applyInlineWrap("==", "=="),
    "insert-table": () => applyBlockInsert(`\n${insertTableTemplate(3, 3)}`),
    "insert-image": () => {
      captureSelection?.();
      onInsertImage?.();
    },
  };
  useEffect(() => {
    if (readOnly) return;
    const onFormat = (event: Event) =>
      menuHandlers.current[(event as CustomEvent<string>).detail]?.();
    window.addEventListener(MARKDOWN_FORMAT_EVENT, onFormat);
    return () => window.removeEventListener(MARKDOWN_FORMAT_EVENT, onFormat);
  }, [readOnly]);

  return (
    <div ref={workspaceRef} className="relative flex h-full min-h-0 min-w-0 flex-col">
      {!chromeHidden && (
        <div data-vr-markdown-hero className="no-print relative z-20 h-0 w-full shrink-0">
          {activeSurface && (
            <aside
              aria-label="Markdown inspector"
              style={{
                top: inspectorTop,
                maxHeight: Math.max(44, workspaceHeight - inspectorTop - 12),
              }}
              className="vr-chrome pointer-events-auto absolute right-3 top-16 z-20 flex w-80 max-w-[calc(100%-1.5rem)] min-h-0 flex-col overflow-hidden rounded-xl border border-line bg-panel/95 text-muted shadow-xl backdrop-blur-sm"
            >
              <header className="flex h-11 shrink-0 items-center justify-between border-b border-line px-4">
                <div className="flex items-center gap-2 font-sans text-xs font-semibold text-paper">
                  <ToolIcon item={activeSurface} size={16} />
                  <span className="capitalize">{activeSurface}</span>
                </div>
                <button
                  type="button"
                  aria-label="Close Markdown inspector"
                  title="Close Markdown inspector"
                  onClick={closeSurface}
                  className="vr-press vr-focus grid h-7 w-7 place-items-center rounded-md text-muted hover:bg-ink hover:text-paper"
                >
                  <X size={14} weight="regular" aria-hidden />
                </button>
              </header>
              <div
                className={`min-h-0 flex-1 overflow-y-auto text-xs ${activeSurface === "insert" ? "p-0" : "p-3"}`}
              >
                {activeSurface === "issues" && (
                  <MarkdownIssuesPanel
                    analysis={analysis}
                    core={core}
                    onJumpToRange={onJumpToRange}
                  />
                )}
                {activeSurface === "properties" && <MarkdownPropertiesPanel core={core} />}
                {activeSurface === "outline" && (
                  <MarkdownOutlineTree
                    nodes={nodes}
                    activeNodeId={activeNodeId}
                    onJump={onJumpToRange}
                  />
                )}
                {activeSurface === "insert" && (
                  <MarkdownInsertMenu
                    core={core}
                    insertAt={getInsertAt()}
                    onInsertImage={onInsertImage}
                    getEditorView={getEditorView}
                  />
                )}
                {activeSurface === "links" && (
                  <MarkdownLinkPanel
                    core={core}
                    documentPath={documentPath}
                    getInsertAt={getInsertAt}
                    selection={selection}
                  />
                )}
              </div>
            </aside>
          )}
        </div>
      )}

      {/* Docked format bar (Edit) + find stack. The bar row sits in flow above the scroll area,
          so scrolled content ends at its hairline instead of sliding under it; find mounts inside
          the row so it hangs 8px under it. In Read the row collapses to zero height and find
          floats at the top of the content. */}
      {!chromeHidden && !readOnly && (
        <MarkdownSelectionBubble
          getEditorView={getEditorView}
          onCommand={(id) => menuHandlers.current[id]?.()}
        />
      )}
      {!chromeHidden && (
        <div className="relative z-30 shrink-0">
          <DocBar>
            {!readOnly && (
              <div className="vr-capsule h-[42px] px-1">
                {/* This panel acts as the Markdown command adapter via shared DocumentToolbarShell —
                    shell = row geometry/spacing/overflow/sticky/a11y/focus; adapter = commands.
                    Proves consistency without merging correctness models. */}
                <DocumentToolbarShell
                  ariaLabel="Markdown format tools"
                  variant="scroll"
                  className="vr-capsule-scroll text-muted"
                >
                  {/* Global formatting toolbar — single owner per docs/plans/2026-08-13-block-local-editing-repair.md; survives focus via captureSelection */}
                  {(() => {
                    const lineReason = getLinePrefixDisabledReason();
                    const headingReason = activeDraft
                      ? "Finish block edit before changing block type"
                      : null;
                    const inlineReason = getInlineWrapDisabledReason();
                    const blockReason = getBlockInsertDisabledReason();
                    const moveReason = activeDraft
                      ? "Finish block edit before moving blocks"
                      : null;
                    const moveSnapshot = core.snapshot();
                    const moveBlockIndex = topLevelMarkdownBlockIndexAt(moveSnapshot, caretOffset);
                    const canMoveUp =
                      !moveReason &&
                      moveBlockIndex !== null &&
                      canMoveMarkdownBlock(moveSnapshot, moveBlockIndex, "up");
                    const canMoveDown =
                      !moveReason &&
                      moveBlockIndex !== null &&
                      canMoveMarkdownBlock(moveSnapshot, moveBlockIndex, "down");
                    const styleLabel = effectiveToolbarState.headingLevel
                      ? `H${effectiveToolbarState.headingLevel}`
                      : "Body";
                    return (
                      <>
                        {/* Wave 2: H1–H6 collapse into one paragraph-style dropdown. */}
                        <Dropdown
                          label={styleLabel}
                          title={headingReason ?? "Paragraph style"}
                          minWidth={60}
                          panelWidth={160}
                          disabled={!!headingReason}
                        >
                          {(close) => (
                            <>
                              <Item
                                active={effectiveToolbarState.headingLevel === 1}
                                onRun={() => {
                                  applyBlockStyle(1);
                                  close();
                                }}
                              >
                                Title
                              </Item>
                              <Item
                                active={effectiveToolbarState.headingLevel === 2}
                                onRun={() => {
                                  applyBlockStyle(2);
                                  close();
                                }}
                              >
                                Heading
                              </Item>
                              <Item
                                active={effectiveToolbarState.headingLevel === 3}
                                onRun={() => {
                                  applyBlockStyle(3);
                                  close();
                                }}
                              >
                                Subheading
                              </Item>
                              <Item
                                active={effectiveToolbarState.headingLevel === null}
                                onRun={() => {
                                  applyBlockStyle(0);
                                  close();
                                }}
                              >
                                Body
                              </Item>
                              <Item
                                disabled={!!lineReason}
                                onRun={() => {
                                  applyLinePrefix("> ");
                                  close();
                                }}
                              >
                                Quote
                              </Item>
                              <Item
                                active={effectiveToolbarState.headingLevel === 4}
                                onRun={() => {
                                  applyBlockStyle(4);
                                  close();
                                }}
                              >
                                Heading 4
                              </Item>
                              <Item
                                active={effectiveToolbarState.headingLevel === 5}
                                onRun={() => {
                                  applyBlockStyle(5);
                                  close();
                                }}
                              >
                                Heading 5
                              </Item>
                              <Item
                                active={effectiveToolbarState.headingLevel === 6}
                                onRun={() => {
                                  applyBlockStyle(6);
                                  close();
                                }}
                              >
                                Heading 6
                              </Item>
                            </>
                          )}
                        </Dropdown>
                        <Sep />
                        <Group>
                          <TBtn
                            label={<TextB size={15} weight="regular" aria-hidden />}
                            title={inlineReason ?? `Bold (${shortcutLabel("mod+b")})`}
                            ariaLabel={`Bold (${shortcutLabel("mod+b")})`}
                            active={effectiveToolbarState.bold}
                            disabled={!!inlineReason}
                            bold
                            round
                            onRun={() => applyInlineWrap("**", "**")}
                          />
                          <TBtn
                            label={<TextItalic size={15} weight="regular" aria-hidden />}
                            title={inlineReason ?? `Italic (${shortcutLabel("mod+i")})`}
                            ariaLabel={`Italic (${shortcutLabel("mod+i")})`}
                            active={effectiveToolbarState.italic}
                            disabled={!!inlineReason}
                            italic
                            round
                            onRun={() => applyInlineWrap("*", "*")}
                          />
                          <TBtn
                            label={<TextStrikethrough size={15} weight="regular" aria-hidden />}
                            title={inlineReason ?? "Strikethrough"}
                            ariaLabel="Strikethrough"
                            active={effectiveToolbarState.strike}
                            disabled={!!inlineReason}
                            strike
                            round
                            onRun={() => applyInlineWrap("~~", "~~")}
                          />
                          <TBtn
                            label={<Code size={15} weight="regular" aria-hidden />}
                            title={inlineReason ?? "Inline code"}
                            ariaLabel="Inline code"
                            active={effectiveToolbarState.code}
                            disabled={!!inlineReason}
                            round
                            onRun={() => applyInlineWrap("`", "`", "code")}
                          />
                          <TBtn
                            label={<Highlighter size={15} weight="regular" />}
                            title={inlineReason ?? "Highlight"}
                            ariaLabel="Highlight"
                            active={effectiveToolbarState.highlight}
                            disabled={!!inlineReason}
                            round
                            onRun={() => applyInlineWrap("==", "==")}
                          />
                        </Group>
                        <Sep />
                        <Group>
                          <TBtn
                            label={<ListBullets size={15} weight="regular" aria-hidden />}
                            title={lineReason ?? "Bullet list"}
                            ariaLabel="Bullet list"
                            disabled={!!lineReason}
                            round
                            onRun={() => applyLinePrefix("- ")}
                          />
                          <TBtn
                            label={<ListNumbers size={15} weight="regular" aria-hidden />}
                            title={lineReason ?? "Ordered list"}
                            ariaLabel="Ordered list"
                            disabled={!!lineReason}
                            round
                            onRun={() => applyLinePrefix("1. ")}
                          />
                          <TBtn
                            label={<CheckSquare size={15} weight="regular" aria-hidden />}
                            title={lineReason ?? "Task list"}
                            ariaLabel="Task list"
                            disabled={!!lineReason}
                            round
                            onRun={() => applyLinePrefix("- [ ] ")}
                          />
                          <TBtn
                            label={<Quotes size={15} weight="regular" aria-hidden />}
                            title={lineReason ?? "Quote"}
                            ariaLabel="Quote"
                            disabled={!!lineReason}
                            round
                            onRun={() => applyLinePrefix("> ")}
                          />
                        </Group>
                        <Sep />
                        <Group>
                          <TBtn
                            label={<ArrowUp size={15} weight="regular" aria-hidden />}
                            title={moveReason ?? "Move block up (⌥↑)"}
                            ariaLabel="Move block up"
                            disabled={!canMoveUp}
                            round
                            onRun={() => moveBlock("up")}
                          />
                          <TBtn
                            label={<ArrowDown size={15} weight="regular" aria-hidden />}
                            title={moveReason ?? "Move block down (⌥↓)"}
                            ariaLabel="Move block down"
                            disabled={!canMoveDown}
                            round
                            onRun={() => moveBlock("down")}
                          />
                        </Group>
                        <Sep />
                        <Group>
                          <TBtn
                            label={<LinkSimple size={15} weight="regular" />}
                            title={inlineReason ?? "Link"}
                            ariaLabel="Link"
                            disabled={!!inlineReason}
                            round
                            onRun={() => applyInlineWrap("[", "](https://)", "text")}
                          />
                          <TBtn
                            label={<ImageSquare size={15} weight="regular" />}
                            title={blockReason ?? "Image"}
                            ariaLabel="Image"
                            disabled={!!blockReason}
                            round
                            onRun={() => {
                              // Unlike the apply* helpers, image insertion has no internal
                              // capture/guard — keep both here so the bookmark and the
                              // block-draft restriction behave exactly as before.
                              captureSelection?.();
                              if (blockReason) return;
                              onInsertImage?.();
                            }}
                          />
                        </Group>
                        {tableContext && (
                          <>
                            <Sep />
                            <MarkdownTableToolbarGroup
                              context={tableContext}
                              getEditorView={() => getEditorView?.() ?? null}
                              onRevealSource={onJumpToRange}
                            />
                          </>
                        )}
                        <Sep />
                        {/* Wave 2: rarely-used block inserts collapse into one "+" menu. */}
                        <Dropdown
                          label={<Plus size={15} weight="regular" />}
                          title={blockReason ?? "Insert block"}
                          minWidth={30}
                          panelWidth={170}
                          disabled={!!blockReason}
                        >
                          {(close) => (
                            <>
                              <Item
                                onRun={() => {
                                  applyBlockInsert("\n```\n\n```\n");
                                  close();
                                }}
                              >
                                Code block
                              </Item>
                              <Item
                                onRun={() => {
                                  applyBlockInsert("\n$$\n\n$$\n");
                                  close();
                                }}
                              >
                                Math block
                              </Item>
                              <Item
                                onRun={() => {
                                  applyBlockInsert("\n```mermaid\ngraph TD\n  A --> B\n```\n");
                                  close();
                                }}
                              >
                                Mermaid diagram
                              </Item>
                              <Item
                                onRun={() => {
                                  applyBlockInsert(`\n${insertTableTemplate(3, 3)}`);
                                  close();
                                }}
                              >
                                Table
                              </Item>
                              <Item
                                onRun={() => {
                                  applyBlockInsert("\n---\n");
                                  close();
                                }}
                              >
                                Divider
                              </Item>
                            </>
                          )}
                        </Dropdown>
                      </>
                    );
                  })()}
                </DocumentToolbarShell>
                {/* Document tools (insert panel, issues, outline, properties, links) share the one
                    floating bar with formatting — no separate row above the document. */}
                <div
                  ref={toolbarRef}
                  role="toolbar"
                  aria-label="Markdown tools"
                  className="flex shrink-0 items-center gap-0.5 border-l border-line pl-1"
                >
                  {!readOnly && (
                    <ToolButton
                      item="insert"
                      active={surface === "insert"}
                      onSelect={selectSurface}
                      buttonRef={(el) => {
                        toolButtonRefs.current.insert = el;
                      }}
                      captureSelection={captureSelection}
                    />
                  )}
                  {(["issues", "outline", "properties", "links"] as const)
                    .filter((item) => !readOnly || READ_SURFACES.includes(item))
                    .map((item) => (
                      <ToolButton
                        key={item}
                        item={item}
                        active={surface === item}
                        onSelect={selectSurface}
                        buttonRef={(el) => {
                          toolButtonRefs.current[item] = el;
                        }}
                        captureSelection={captureSelection}
                      />
                    ))}
                </div>
              </div>
            )}
            <DocumentToolbarSearch
              findBar={findBar}
              activeMode={findMode}
              onRequestFind={onRequestFind}
              onRequestReplace={onRequestReplace}
            />
          </DocBar>
        </div>
      )}

      <div
        ref={scrollContainerRef}
        data-document-scroll=""
        onScroll={handleScroll}
        className="vr-reading-container min-h-0 min-w-0 flex-1 overflow-y-auto overflow-x-hidden"
      >
        {children}
      </div>
      {showBackToTop && <BackToTopButton onClick={scrollToTop} />}
    </div>
  );
}

function ToolButton({
  item,
  active,
  onSelect,
  buttonRef,
  captureSelection,
}: {
  item: WorkspaceSurface;
  active: boolean;
  onSelect: (item: WorkspaceSurface) => void;
  buttonRef: (el: HTMLButtonElement | null) => void;
  captureSelection?: () => MarkdownSelectionRange | undefined;
}): ReactNode {
  const label = item[0].toUpperCase() + item.slice(1);
  return (
    <button
      ref={buttonRef}
      onPointerDown={(event) => {
        // Preserve ProseMirror selection + focus bookmark over toolbar pointer
        // interaction. Both pointer and mouse events can steal focus depending
        // on input device; handling both keeps the bookmark byte-identical.
        event.preventDefault();
        captureSelection?.();
      }}
      onMouseDown={(event) => {
        event.preventDefault();
        captureSelection?.();
      }}
      type="button"
      aria-label={label}
      aria-pressed={active}
      title={`${active ? "Close" : "Open"} ${item}`}
      onClick={() => onSelect(item)}
      className={`vr-press vr-focus grid h-8 w-8 shrink-0 place-items-center rounded-md border ${
        active
          ? "border-copper/60 bg-copper/15 text-paper shadow-sm"
          : "border-transparent text-muted hover:border-line hover:bg-ink/70 hover:text-paper"
      }`}
    >
      <ToolIcon item={item} active={active} size={16} />
    </button>
  );
}

const TOOL_ICONS: Record<WorkspaceSurface, Icon> = {
  insert: ListBullets,
  issues: WarningCircle,
  outline: TreeStructure,
  properties: SlidersHorizontal,
  links: LinkSimple,
};

function ToolIcon({
  item,
  active,
  size,
}: {
  item: WorkspaceSurface;
  active?: boolean;
  size: number;
}): ReactNode {
  const IconComponent = TOOL_ICONS[item];
  return <IconComponent size={size} weight={active ? "fill" : "regular"} aria-hidden />;
}

function MarkdownOutlineTree({
  nodes,
  activeNodeId,
  onJump,
}: {
  nodes: readonly OutlineNode[];
  activeNodeId: string | null;
  onJump: (range: { from: number; to: number }) => void;
}): ReactNode {
  if (nodes.length === 0) return <div data-testid="markdown-outline-empty">No headings</div>;
  return (
    <nav aria-label="Document outline" data-testid="markdown-outline">
      {nodes.map((node) => (
        <OutlineItem key={node.id} node={node} activeNodeId={activeNodeId} onJump={onJump} />
      ))}
    </nav>
  );
}

function OutlineItem({
  node,
  activeNodeId,
  onJump,
}: {
  node: OutlineNode;
  activeNodeId: string | null;
  onJump: (range: { from: number; to: number }) => void;
}): ReactNode {
  return (
    <div className="min-w-0" style={{ paddingLeft: Math.max(0, node.level - 1) * 10 }}>
      <button
        type="button"
        aria-current={activeNodeId === node.id ? "location" : undefined}
        onClick={() => onJump(node.range)}
        className="w-full truncate text-left text-xs hover:text-paper"
      >
        {node.kind === "heading" ? `H${node.level} ` : ""}
        {node.title}
      </button>
      {node.children.map((child) => (
        <OutlineItem key={child.id} node={child} activeNodeId={activeNodeId} onJump={onJump} />
      ))}
    </div>
  );
}
