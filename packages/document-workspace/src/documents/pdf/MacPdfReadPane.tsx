import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { logError } from "@rightkit/document-core/state/diagnostics";
import { PdfPasswordPrompt } from "@rightkit/pdf-ui";
import { PdfToolbar } from "./PdfToolbar.js";
import { ZOOM_MAX, ZOOM_MIN } from "./pdfShared.js";
import type { SearchOptions, SearchProvider, ViewMeta } from "@rightkit/document-core/documents/types";
import {
  listenNativePdfKey,
  listenNativePdfState,
  nativePdfClose,
  nativePdfFind,
  nativePdfOpen,
  nativePdfPrint,
  nativePdfState,
  nativePdfSuspend,
  nativePdfUpdate,
  type NativePdfFrame,
  type NativePdfEditingAction,
  type NativePdfKeyEvent,
  type NativePdfOutlineNode,
  type NativePdfState,
} from "./macPdfNativeBridge.js";
import { hostId } from "@rightkit/document-core/host";
export type { NativePdfEditingAction } from "./macPdfNativeBridge.js";

export interface MacPdfReadPaneProps {
  documentPath: string;
  url?: string;
  active?: boolean;
  initialView?: ViewMeta;
  onViewState?: (view: ViewMeta) => void;
  registerPrint?: (fn: (() => Promise<boolean>) | null) => void;
  registerSearchProvider?: (provider: SearchProvider | null) => void;
  findBar?: ReactNode;
  findMode?: "find" | "replace";
  onRequestFind?: () => void;
  onRequestReplace?: () => void;
  onRequestEditing: (action: NativePdfEditingAction, view: ViewMeta) => void;
  onRequestFallback: (error: Error) => void;
  /** Wave 2 Read mode — hides every markup affordance in the shared PdfToolbar. */
  readOnly?: boolean;
}

let ownerSequence = 0;

function nextOwner(): string {
  ownerSequence += 1;
  return `${hostId()}-native-pdf-${ownerSequence}`;
}

function asError(error: unknown): Error {
  return error instanceof Error ? error : new Error(String(error));
}

function frameFor(element: HTMLElement | null): NativePdfFrame {
  if (!element || typeof window === "undefined") return { x: 0, y: 0, width: 0, height: 0 };
  const rect = element.getBoundingClientRect();
  const viewportWidth = Math.max(0, window.innerWidth || document.documentElement.clientWidth || 0);
  const viewportHeight = Math.max(
    0,
    window.innerHeight || document.documentElement.clientHeight || 0,
  );
  const x = Math.min(viewportWidth, Math.max(0, rect.left));
  const y = Math.min(viewportHeight, Math.max(0, rect.top));
  return {
    x,
    y,
    width: Math.max(0, Math.min(rect.width, viewportWidth - x)),
    height: Math.max(0, Math.min(rect.height, viewportHeight - y)),
  };
}

type ReaderRgb = [number, number, number];

function readerRgbFor(element: HTMLElement | null): ReaderRgb | undefined {
  if (typeof document === "undefined" || typeof window === "undefined") return undefined;
  const host = element ?? document.documentElement;
  const raw =
    window.getComputedStyle(host).getPropertyValue("--reader-surface-rgb").trim() ||
    window
      .getComputedStyle(document.documentElement)
      .getPropertyValue("--reader-surface-rgb")
      .trim();
  const values = raw.split(/\s+/).map(Number);
  if (values.length !== 3 || values.some((value) => !Number.isFinite(value))) return undefined;
  return values.map((value) => Math.max(0, Math.min(255, Math.round(value)))) as ReaderRgb;
}

function stateView(state: NativePdfState, panel: "outline" | "thumbs" | null): ViewMeta {
  return {
    page: state.page,
    scale: state.scale,
    fit: state.fit,
    rotation: state.rotation,
    pdfPanel: panel,
  };
}

function visibleElement(element: Element): boolean {
  if (!(element instanceof HTMLElement)) return false;
  const style = window.getComputedStyle(element);
  if (style.display === "none" || style.visibility === "hidden" || style.opacity === "0") {
    return false;
  }
  const rect = element.getBoundingClientRect();
  return rect.width > 0 && rect.height > 0;
}

function isOverlayElement(element: Element | null): boolean {
  if (!element || !visibleElement(element)) return false;
  const role = element.getAttribute("role");
  const overlay =
    role === "dialog" ||
    role === "alertdialog" ||
    role === "menu" ||
    element.getAttribute("aria-modal") === "true";
  if (!overlay) return false;
  // Toolbar itself remains web-owned. Menus/dialogs nested in it still occlude the native view.
  if (element.classList.contains("vr-document-toolbar")) return false;
  return true;
}

function hasOccludingOverlay(host: HTMLElement): boolean {
  const hostRect = host.getBoundingClientRect();
  if (hostRect.width <= 0 || hostRect.height <= 0) return false;
  const overlaps = (element: Element) => {
    if (!isOverlayElement(element)) return false;
    const rect = (element as HTMLElement).getBoundingClientRect();
    return (
      rect.left < hostRect.right &&
      rect.right > hostRect.left &&
      rect.top < hostRect.bottom &&
      rect.bottom > hostRect.top
    );
  };
  if (
    Array.from(
      document.querySelectorAll(
        '[role="dialog"], [role="alertdialog"], [role="menu"], [aria-modal="true"]',
      ),
    ).some(overlaps)
  ) {
    return true;
  }
  // elementFromPoint catches portaled/popover content whose semantic node is not in the expected
  // DOM subtree. Sample center and corners, while leaving toolbar pixels out of the samples.
  const points = [
    [hostRect.left + hostRect.width / 2, hostRect.top + hostRect.height / 2],
    [hostRect.left + 2, hostRect.top + 2],
    [hostRect.right - 2, hostRect.bottom - 2],
  ];
  return points.some(([x, y]) => {
    const element = document.elementFromPoint(x, y);
    return isOverlayElement(element);
  });
}

function NativeOutline({
  nodes,
  depth = 0,
  onSelect,
}: {
  nodes: NativePdfOutlineNode[];
  depth?: number;
  onSelect: (page: number) => void;
}) {
  return (
    <ul>
      {nodes.map((node, index) => (
        <li key={`${node.title}-${index}`}>
          <button
            type="button"
            disabled={node.page === null}
            onClick={() => node.page !== null && onSelect(node.page)}
            className="flex w-full truncate py-1 pr-2 text-left text-xs text-muted hover:text-paper disabled:opacity-60"
            style={{ paddingLeft: depth * 12 + 8 }}
            title={node.title}
          >
            {node.title || "Untitled"}
          </button>
          {node.children.length > 0 && (
            <NativeOutline nodes={node.children} depth={depth + 1} onSelect={onSelect} />
          )}
        </li>
      ))}
    </ul>
  );
}

export function MacPdfReadPane({
  documentPath,
  active = true,
  initialView,
  onViewState,
  registerPrint,
  registerSearchProvider,
  findBar,
  findMode,
  onRequestFind,
  onRequestReplace,
  onRequestEditing,
  onRequestFallback,
  readOnly = false,
}: MacPdfReadPaneProps) {
  const hostRef = useRef<HTMLDivElement>(null);
  const ownerRef = useRef<string>(nextOwner());
  const activeRef = useRef(active);
  activeRef.current = active;
  const initialViewRef = useRef(initialView);
  initialViewRef.current = initialView;
  const onViewStateRef = useRef(onViewState);
  onViewStateRef.current = onViewState;
  const onFallbackRef = useRef(onRequestFallback);
  onFallbackRef.current = onRequestFallback;
  const onEditingRef = useRef(onRequestEditing);
  onEditingRef.current = onRequestEditing;
  const [state, setState] = useState<NativePdfState | null>(null);
  const stateRef = useRef<NativePdfState | null>(null);
  const [loaded, setLoaded] = useState(false);
  const loadedRef = useRef(false);
  const initialPanel = initialView?.pdfPanel ?? null;
  const [panel, setPanel] = useState<null | "outline" | "thumbs">(initialPanel);
  const panelRef = useRef<null | "outline" | "thumbs">(initialPanel);
  panelRef.current = panel;
  const pathRef = useRef(documentPath);
  pathRef.current = documentPath;
  const [pageDraft, setPageDraft] = useState<string | null>(null);
  const [scaleDraft, setScaleDraft] = useState("100");
  const [passwordRetry, setPasswordRetry] = useState<boolean | null>(null);
  const [passwordCanceled, setPasswordCanceled] = useState(false);
  const [openNonce, setOpenNonce] = useState(0);
  const openNonceRef = useRef(openNonce);
  openNonceRef.current = openNonce;
  const [error, setError] = useState<Error | null>(null);
  const [editingFailure, setEditingFailure] = useState<NativePdfEditingAction | null>(null);
  const generationRef = useRef(0);
  const fallbackSentRef = useRef(false);
  const updateSequenceRef = useRef(0);
  const searchSequenceRef = useRef(0);
  const searchResultRef = useRef({ count: 0, activeIndex: -1 });
  const coveredRef = useRef(false);
  const lastStateEventAtRef = useRef(0);
  const statePollSequenceRef = useRef(0);
  const editingPendingRef = useRef(false);
  const nativeKeyHandlerRef = useRef<(event: NativePdfKeyEvent) => void>(() => undefined);

  const acceptState = useCallback((next: NativePdfState, fromEvent = false) => {
    if (!activeRef.current || next.owner !== ownerRef.current) return;
    if (fromEvent) lastStateEventAtRef.current = Date.now();
    stateRef.current = next;
    setState(next);
    // Native state can report null after a parked reader expires. Keep selected panel from
    // lightweight persisted/local view state; explicit hide updates panelRef before IPC, so null
    // remains authoritative when user closed panel deliberately.
    const restoredPanel =
      next.panel === "thumbs" || next.panel === "outline"
        ? next.panel
        : next.panel === null || next.panel === undefined
          ? panelRef.current
          : null;
    panelRef.current = restoredPanel;
    setPanel(restoredPanel);
    setScaleDraft(String(Math.round(next.scale * 100)));
    setPageDraft(null);
    setLoaded(!next.locked);
    loadedRef.current = !next.locked;
    if (next.locked) setPasswordRetry((retry) => retry ?? false);
    else setPasswordRetry(null);
    onViewStateRef.current?.(stateView(next, restoredPanel));
  }, []);

  const reportFallback = useCallback((cause: unknown) => {
    const next = asError(cause);
    if (!fallbackSentRef.current) {
      fallbackSentRef.current = true;
      const owner = ownerRef.current;
      const generation = generationRef.current;
      void nativePdfClose(owner, pathRef.current, "fallback")
        .catch(() => undefined)
        .finally(() => {
          if (
            !activeRef.current ||
            owner !== ownerRef.current ||
            generation !== generationRef.current
          )
            return;
          setError(next);
          onFallbackRef.current(next);
        });
    }
  }, []);

  const sendUpdate = useCallback(
    async (args: Parameters<typeof nativePdfUpdate>[0]) => {
      if (!activeRef.current || editingPendingRef.current || args.owner !== ownerRef.current)
        return;
      const sequence = ++updateSequenceRef.current;
      try {
        const next = await nativePdfUpdate(args);
        if (
          activeRef.current &&
          loadedRef.current &&
          !editingPendingRef.current &&
          args.owner === ownerRef.current &&
          sequence === updateSequenceRef.current
        ) {
          acceptState(next);
        }
      } catch (cause) {
        if (
          activeRef.current &&
          loadedRef.current &&
          !editingPendingRef.current &&
          args.owner === ownerRef.current &&
          sequence === updateSequenceRef.current
        ) {
          reportFallback(cause);
        }
      }
    },
    [acceptState, reportFallback],
  );

  const currentFrame = useCallback(() => frameFor(hostRef.current), []);

  // Subscribe once per pane. Owner filtering makes late events from a released native document
  // harmless, including events delivered after a StrictMode effect cleanup.
  useEffect(() => {
    let canceled = false;
    let unlistenState: (() => void) | undefined;
    let unlistenKey: (() => void) | undefined;
    const dispose = (off: (() => void) | undefined) => {
      if (!off) return;
      try {
        const result = (off as unknown as () => unknown)();
        if (result && typeof (result as { then?: unknown }).then === "function") {
          void Promise.resolve(result).catch(() => undefined);
        }
      } catch {
        // Listener teardown is best-effort during StrictMode/HMR and window shutdown.
      }
    };
    void listenNativePdfState((next) => {
      if (!canceled) acceptState(next, true);
    }).then(
      (off) => {
        if (canceled) dispose(off);
        else unlistenState = off;
      },
      () => undefined,
    );
    void listenNativePdfKey((event) => {
      if (!canceled && activeRef.current && (!event.owner || event.owner === ownerRef.current)) {
        nativeKeyHandlerRef.current(event);
      }
    }).then(
      (off) => {
        if (canceled) dispose(off);
        else unlistenKey = off;
      },
      () => undefined,
    );
    return () => {
      canceled = true;
      dispose(unlistenState);
      dispose(unlistenKey);
    };
  }, [acceptState]);

  // Open/close lifecycle. Each document generation gets its own owner, so an old event cannot
  // attach to a replacement document. Cleanup is queued by the bridge before any next open.
  useEffect(() => {
    void openNonce;
    const owner = nextOwner();
    ownerRef.current = owner;
    const generation = ++generationRef.current;
    let canceled = false;
    fallbackSentRef.current = false;
    updateSequenceRef.current += 1;
    setState(null);
    stateRef.current = null;
    setLoaded(false);
    loadedRef.current = false;
    setError(null);
    setEditingFailure(null);
    setPasswordRetry(null);
    setPasswordCanceled(false);
    editingPendingRef.current = false;

    const open = async (password?: string) => {
      try {
        const readerRgb = readerRgbFor(hostRef.current);
        const next = await nativePdfOpen({
          owner,
          path: documentPath,
          frame: currentFrame(),
          ...(readerRgb ? { readerRgb } : {}),
          initialView: {
            page: initialViewRef.current?.page,
            scale: initialViewRef.current?.scale,
            fit: initialViewRef.current?.fit,
            rotation: initialViewRef.current?.rotation,
          },
          ...(password === undefined ? {} : { password }),
        });
        if (canceled || generation !== generationRef.current || owner !== ownerRef.current) {
          await nativePdfClose(owner, documentPath, "reload");
          return;
        }
        acceptState(next);
        const selectedPanel = panelRef.current;
        if (selectedPanel) {
          try {
            const restored = await nativePdfUpdate({ owner, panel: selectedPanel });
            if (!canceled && generation === generationRef.current && owner === ownerRef.current) {
              acceptState(restored);
            }
          } catch (restoreCause) {
            if (!canceled && generation === generationRef.current && owner === ownerRef.current) {
              reportFallback(restoreCause);
            }
          }
        }
      } catch (cause) {
        if (!canceled && generation === generationRef.current && owner === ownerRef.current) {
          reportFallback(cause);
        }
      }
    };

    if (active) void open();

    return () => {
      canceled = true;
      updateSequenceRef.current += 1;
      setLoaded(false);
      loadedRef.current = false;
      const reloading = pathRef.current !== documentPath || openNonceRef.current !== openNonce;
      if (reloading) {
        void nativePdfClose(owner, documentPath, "reload").catch(() => undefined);
      } else if (active) {
        void nativePdfSuspend(owner, documentPath).catch(() => undefined);
      }
    };
  }, [acceptState, currentFrame, documentPath, active, openNonce, reportFallback]);

  // Pane unmount can mean LRU shell eviction while its document tab remains open. The lifecycle
  // cleanup above suspends active ownership; app-level tab-close code must call nativePdfClosePath
  // when document is genuinely removed so parked readers survive cold remount without leaking.

  const handlePasswordSubmit = useCallback(
    (password: string) => {
      const owner = ownerRef.current;
      const generation = generationRef.current;
      setPasswordRetry(null);
      const readerRgb = readerRgbFor(hostRef.current);
      void nativePdfOpen({
        owner,
        path: documentPath,
        frame: currentFrame(),
        ...(readerRgb ? { readerRgb } : {}),
        initialView: {
          page: initialViewRef.current?.page,
          scale: initialViewRef.current?.scale,
          fit: initialViewRef.current?.fit,
          rotation: initialViewRef.current?.rotation,
        },
        password,
      })
        .then(async (next) => {
          if (
            activeRef.current &&
            generation === generationRef.current &&
            owner === ownerRef.current
          ) {
            acceptState(next);
            const selectedPanel = panelRef.current;
            if (selectedPanel) {
              const restored = await nativePdfUpdate({ owner, panel: selectedPanel });
              if (
                activeRef.current &&
                generation === generationRef.current &&
                owner === ownerRef.current
              ) {
                acceptState(restored);
              }
            }
          } else {
            void nativePdfClose(owner, documentPath, "terminal-close");
          }
        })
        .catch(() => {
          if (
            activeRef.current &&
            generation === generationRef.current &&
            owner === ownerRef.current
          ) {
            setPasswordRetry(true);
          }
        });
    },
    [acceptState, currentFrame, documentPath],
  );

  const handlePasswordCancel = useCallback(() => {
    setPasswordRetry(null);
    setPasswordCanceled(true);
    setLoaded(false);
    loadedRef.current = false;
    void nativePdfClose(ownerRef.current, documentPath, "terminal-close").catch(() => undefined);
  }, [documentPath]);

  type NativePdfChange = Omit<Parameters<typeof nativePdfUpdate>[0], "owner">;
  const updateView = useCallback(
    (change: NativePdfChange) => {
      void sendUpdate({ owner: ownerRef.current, ...change });
    },
    [sendUpdate],
  );

  const commitPage = useCallback(() => {
    if (pageDraft === null || pageDraft.trim() === "") {
      setPageDraft(null);
      return;
    }
    const raw = Number(pageDraft);
    const next = stateRef.current;
    if (!next || !Number.isFinite(raw)) {
      setPageDraft(null);
      return;
    }
    const page = Math.max(1, Math.min(next.numPages, Math.round(raw)));
    updateView({ page });
    setPageDraft(null);
  }, [pageDraft, updateView]);

  const setScale = useCallback(
    (value: number) => {
      const scale = Math.max(ZOOM_MIN, Math.min(ZOOM_MAX, value));
      updateView({ scale, fit: "" });
    },
    [updateView],
  );

  const commitScale = useCallback(() => {
    const raw = Number(scaleDraft);
    if (!Number.isFinite(raw)) {
      setScaleDraft(String(Math.round((stateRef.current?.scale ?? 1) * 100)));
      return;
    }
    setScale(raw / 100);
  }, [scaleDraft, setScale]);

  const changePanel = useCallback(
    (next: "outline" | "thumbs") => {
      const value = panel === next ? null : next;
      panelRef.current = value;
      setPanel(value);
      updateView({ panel: value ?? "" });
    },
    [panel, updateView],
  );

  const zoom = useCallback(
    (delta: number) => setScale((stateRef.current?.scale ?? 1) + delta),
    [setScale],
  );

  const rotate = useCallback(
    (delta: number) => {
      const current = stateRef.current?.rotation ?? 0;
      updateView({ rotation: (((current + delta) % 360) + 360) % 360 });
    },
    [updateView],
  );

  const chooseFit = useCallback((fit: "width" | "page") => updateView({ fit }), [updateView]);

  const editingView = useCallback(() => {
    const next = stateRef.current;
    return (
      (next ? stateView(next, panelRef.current) : initialViewRef.current) ?? {
        page: 1,
        scale: 1,
        fit: null,
        rotation: 0,
      }
    );
  }, []);

  const requestEditing = useCallback(
    (action: NativePdfEditingAction) => {
      if (!activeRef.current || !loadedRef.current) return;
      const owner = ownerRef.current;
      const view = editingView();
      if (editingPendingRef.current) return;
      editingPendingRef.current = true;
      updateSequenceRef.current += 1;
      setEditingFailure(null);
      // React must not mount PDF.js's editor/OCR surface while PDFKit still owns a window child.
      // The bridge queue also makes this safe when a document switch is already closing the view.
      void nativePdfClose(owner, documentPath, "edit-handoff")
        .then(() => {
          if (activeRef.current && owner === ownerRef.current) {
            loadedRef.current = false;
            onEditingRef.current(action, view);
          }
        })
        .catch(() => {
          if (activeRef.current && owner === ownerRef.current) {
            logError("pdf:native-edit-transition-failed");
            setEditingFailure(action);
          }
        })
        .finally(() => {
          editingPendingRef.current = false;
        });
    },
    [editingView, documentPath],
  );

  const handleNativeKey = useCallback(
    (event: NativePdfKeyEvent) => {
      if (typeof window === "undefined") return;
      if (!activeRef.current || !stateRef.current) return;
      if (typeof event.code !== "string") return;
      const keyEvent = new KeyboardEvent("keydown", {
        key: event.key,
        code: event.code,
        metaKey: event.metaKey,
        ctrlKey: event.ctrlKey,
        altKey: event.altKey,
        shiftKey: event.shiftKey,
        bubbles: true,
        cancelable: true,
      });
      window.dispatchEvent(keyEvent);
      const command = event.metaKey || event.ctrlKey;
      // Global shortcut handling may cancel Cmd/Ctrl+0; native view reset must still reach PDFKit.
      if (command && event.key === "0") {
        updateView({ scale: 1, fit: "" });
        return;
      }
      if (keyEvent.defaultPrevented) return;
      if (command && (event.key === "+" || event.key === "=")) zoom(0.15);
      else if (command && event.key === "-") zoom(-0.15);
      else if (!command && event.key === "Home") updateView({ page: 1 });
      else if (!command && event.key === "End") updateView({ page: stateRef.current.numPages });
    },
    [updateView, zoom],
  );
  nativeKeyHandlerRef.current = handleNativeKey;

  // Keep native view geometry synchronized with CSS layout. A zero-area frame is explicitly
  // hidden, which avoids a transient native paint over modal teardown/layout transitions.
  useEffect(() => {
    if (!active) return;
    const host = hostRef.current;
    if (!host) return;
    const syncFrame = () => {
      const frame = frameFor(host);
      coveredRef.current = hasOccludingOverlay(host);
      const visible = frame.width > 0 && frame.height > 0 && !coveredRef.current;
      const readerRgb = readerRgbFor(host);
      void sendUpdate({
        owner: ownerRef.current,
        frame,
        visible,
        ...(readerRgb ? { readerRgb } : {}),
      });
    };
    syncFrame();
    const resize = typeof ResizeObserver !== "undefined" ? new ResizeObserver(syncFrame) : null;
    resize?.observe(host);
    window.addEventListener("resize", syncFrame);
    return () => {
      resize?.disconnect();
      window.removeEventListener("resize", syncFrame);
    };
  }, [active, sendUpdate]);

  // Backend state events are preferred, but a window transition can lose one. Poll only after
  // 500ms without an event, with owner/generation guards so stale reads cannot repaint a new pane.
  useEffect(() => {
    if (!active || !loaded) return;
    const owner = ownerRef.current;
    const generation = generationRef.current;
    const sequence = ++statePollSequenceRef.current;
    const poll = async () => {
      if (Date.now() - lastStateEventAtRef.current < 500) return;
      try {
        const next = await nativePdfState(owner);
        if (
          activeRef.current &&
          owner === ownerRef.current &&
          generation === generationRef.current &&
          sequence === statePollSequenceRef.current
        ) {
          acceptState(next);
        }
      } catch {
        // Open/update paths report errors; polling is recovery-only while a stale host is released.
      }
    };
    const timer = window.setInterval(() => void poll(), 500);
    return () => {
      statePollSequenceRef.current += 1;
      window.clearInterval(timer);
    };
  }, [acceptState, active, loaded]);

  useEffect(() => {
    if (!active || typeof document === "undefined") return;
    const host = hostRef.current;
    if (!host) return;
    const checkCoverage = () => {
      const covered = hasOccludingOverlay(host);
      if (covered === coveredRef.current) return;
      coveredRef.current = covered;
      const frame = frameFor(host);
      const readerRgb = readerRgbFor(host);
      void sendUpdate({
        owner: ownerRef.current,
        frame,
        visible: !covered && frame.width > 0 && frame.height > 0,
        ...(readerRgb ? { readerRgb } : {}),
      });
    };
    checkCoverage();
    const observer =
      typeof MutationObserver !== "undefined" ? new MutationObserver(checkCoverage) : null;
    observer?.observe(document.body, {
      subtree: true,
      childList: true,
      attributes: true,
      attributeFilter: ["role", "aria-modal", "style", "class"],
    });
    const themeObserver =
      typeof MutationObserver !== "undefined"
        ? new MutationObserver(() => {
            const readerRgb = readerRgbFor(hostRef.current);
            if (readerRgb) void sendUpdate({ owner: ownerRef.current, readerRgb });
          })
        : null;
    themeObserver?.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ["style"],
    });
    return () => {
      observer?.disconnect();
      themeObserver?.disconnect();
    };
  }, [active, sendUpdate]);

  useEffect(() => {
    if (!registerPrint) return;
    if (!active || !loaded) {
      registerPrint(null);
      return;
    }
    const owner = ownerRef.current;
    const print = () => nativePdfPrint(owner);
    registerPrint(print);
    return () => registerPrint(null);
  }, [active, loaded, registerPrint]);

  useEffect(() => {
    if (!registerSearchProvider) return;
    if (!active || !loaded) {
      registerSearchProvider(null);
      return;
    }
    const owner = ownerRef.current;
    const request = async (args: Parameters<typeof nativePdfFind>[0]) => {
      const sequence = ++searchSequenceRef.current;
      try {
        const next = await nativePdfFind(args);
        if (
          sequence !== searchSequenceRef.current ||
          !activeRef.current ||
          owner !== ownerRef.current
        ) {
          return searchResultRef.current;
        }
        searchResultRef.current = next;
        return next;
      } catch {
        return searchResultRef.current;
      }
    };
    const provider: SearchProvider = {
      kind: "pdf",
      canReplace: false,
      supportsMatchCase: true,
      supportsWholeWord: true,
      find: (query: string, options: SearchOptions) =>
        request({
          owner,
          query,
          matchCase: options.matchCase,
          wholeWord: options.wholeWord,
          clear: !query,
        }),
      next: () =>
        request({
          owner,
          direction: 1,
          index:
            searchResultRef.current.activeIndex >= 0
              ? searchResultRef.current.activeIndex
              : undefined,
        }),
      prev: () =>
        request({
          owner,
          direction: -1,
          index:
            searchResultRef.current.activeIndex >= 0
              ? searchResultRef.current.activeIndex
              : undefined,
        }),
      seek: (matchIndex: number) => {
        if (matchIndex >= 0) void request({ owner, index: matchIndex });
      },
      close: () => {
        searchSequenceRef.current += 1;
        void nativePdfFind({ owner, clear: true });
      },
    };
    registerSearchProvider(provider);
    return () => {
      searchSequenceRef.current += 1;
      registerSearchProvider(null);
    };
  }, [active, loaded, registerSearchProvider]);

  const outline = useMemo(() => state?.outline ?? [], [state?.outline]);
  const current = state?.page ?? initialView?.page ?? 1;
  const fit = state?.fit ?? initialView?.fit ?? null;

  return (
    <div className="relative flex h-full flex-col" data-vr-native-pdf-pane="">
      <div data-vr-native-pdf-toolbar="">
        {/* The capsule is docked in its own in-flow row, so the PDFKit host (a native sibling view
            layered above the webview; frameFor mirrors hostRef's rect) starts below the row's
            hairline instead of covering the bar. */}
        <div className="relative shrink-0">
          <PdfToolbar
            panel={panel}
            hasOutline={state?.hasOutline ?? outline.length > 0}
            current={current}
            numPages={state?.numPages ?? 0}
            pageDraft={pageDraft}
            pageShake={false}
            scaleDraft={scaleDraft}
            fitMode={fit}
            onTogglePanel={changePanel}
            onPageDraftChange={setPageDraft}
            onCommitPageDraft={commitPage}
            onZoom={zoom}
            onScaleDraftChange={setScaleDraft}
            onCommitScaleDraft={commitScale}
            onSetFit={chooseFit}
            onRotate={rotate}
            ocrAvailable
            currentPageScanned
            onRecognize={() => requestEditing({ kind: "ocr" })}
            onRecognizeDocument={() => requestEditing({ kind: "ocr-document" })}
            findBar={findBar}
            findMode={findMode}
            onRequestFind={onRequestFind}
            onRequestReplace={onRequestReplace}
            readOnly={readOnly}
            editorMode="none"
            editorReady={loaded}
            onSetEditorMode={(mode) => {
              if (mode !== "none") requestEditing({ kind: "annotation", mode });
            }}
            onAddImageStamp={(file) => {
              requestEditing({ kind: "stamp", file });
              return undefined;
            }}
            textEditActive={false}
            onToggleTextEdit={() => requestEditing({ kind: "text" })}
          />
        </div>
        {editingFailure && (
          <div role="alert" className="flex items-center gap-2 px-3 py-2 text-sm text-copper-hi">
            Couldn’t open PDF editing.
            <button
              type="button"
              className="underline"
              onClick={() => requestEditing(editingFailure)}
            >
              Retry editing
            </button>
          </div>
        )}
      </div>

      <div className="relative flex min-h-0 flex-1">
        {panel && (
          <aside
            className={`no-print shrink-0 overflow-hidden border-r border-line bg-panel ${panel === "outline" ? "w-64" : "w-44"}`}
            aria-label={panel === "outline" ? "PDF outline" : "PDF thumbnails"}
          >
            {panel === "outline" ? (
              <div className="h-full overflow-auto py-2">
                <NativeOutline nodes={outline} onSelect={(page) => updateView({ page })} />
              </div>
            ) : (
              <div className="h-full" aria-hidden="true" />
            )}
          </aside>
        )}
        <div
          ref={hostRef}
          role="region"
          aria-label="PDF pages"
          data-vr-native-pdf-host=""
          className="relative min-h-0 min-w-0 flex-1 overflow-hidden bg-[var(--reader-surface)]"
        >
          {error && (
            <div role="alert" className="m-4 text-sm text-copper-hi">
              PDF error: {error.message}
            </div>
          )}
          {!loaded && !error && passwordRetry === null && !passwordCanceled && (
            <div className="flex h-full items-center justify-center text-sm text-muted-2">
              Opening PDF…
            </div>
          )}
          {!loaded && !error && passwordCanceled && (
            <div className="flex h-full items-center justify-center">
              <button
                type="button"
                className="rounded border border-line px-3 py-1.5 text-sm text-muted hover:text-paper"
                onClick={() => {
                  setPasswordCanceled(false);
                  setOpenNonce((value) => value + 1);
                }}
              >
                Open PDF
              </button>
            </div>
          )}
        </div>
      </div>

      {passwordRetry !== null && (
        <PdfPasswordPrompt
          retry={passwordRetry}
          onSubmit={handlePasswordSubmit}
          onCancel={handlePasswordCancel}
        />
      )}
    </div>
  );
}

export default MacPdfReadPane;
