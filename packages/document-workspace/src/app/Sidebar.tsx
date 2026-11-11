// The sidebar shell. Open documents moved to the titlebar tab strip — the sidebar is now a file
// explorer (Pinned / Recent / Locations). Collapsed shows a thin rail (expand, New, Recent, pinned
// files, Settings); documentEvent("toggle-sidebar") + "vr.sidebar.pinned" still drive it.
import {
  memo,
  useEffect,
  useRef,
  useState,
  type PointerEvent as ReactPointerEvent,
  type ReactNode,
} from "react";
import { useMediaQuery } from "@rightkit/react-shell/react";
import type { FileEntry, PinEntry } from "../state/library.js";
import { loadExplorerPersisted, saveExplorerPersisted } from "./sidebar/explorer/explorerState.js";
import { SidebarExpandedPanel } from "./sidebar/SidebarExpandedPanel.js";
import { SidebarRail } from "./sidebar/SidebarRail.js";
import { documentEvent } from "@rightkit/document-core/host";

export interface SidebarProps {
  activePath: string | null;
  /** Paths of documents that currently have a tab — used for "open in a tab" row styling and for
   *  activate-instead-of-open when a pinned/explorer file is already open. */
  openPaths: ReadonlySet<string>;
  recents: FileEntry[];
  /** File+dir pins (integrator keeps it current, dirs included). */
  pinnedPaths: ReadonlySet<string>;
  /** Full pin rows for the Pinned section (dirs first, as the backend returns them). */
  pinnedEntries: PinEntry[];
  /** Focus an already-open doc's tab. */
  onActivate: (path: string) => void;
  /** Open a recents entry — same path the recents flow uses (token-based). */
  onOpenRecent: (entry: FileEntry) => Promise<unknown> | undefined;
  /** Open a broker-granted explorer path (integrator makes it open-or-activate). */
  onOpenPath: (path: string) => void;
  /** FILE pin toggle (existing app callback). */
  onTogglePin: (path: string) => void;
  /** DIRECTORY pin toggle. */
  onTogglePinDir: (path: string, label: string) => void;
  onClearRecents: () => void;
  onOpenDialog: () => void;
  onOpenUrl: () => void;
  onOpenClipboard: () => void;
  onNew: () => void;
  onOpenSettings: () => void;
  onCollapsedChange: (collapsed: boolean) => void;
  /** In-window menus (Windows/Linux File · Edit · View) shown at the left of the sidebar's
   *  titlebar row while the sidebar is open — the window's far left, like macOS traffic lights. */
  titlebarSlot?: ReactNode;
}

const SIDEBAR_DEFAULT_WIDTH = 256;
const SIDEBAR_MIN_WIDTH = 224;
const SIDEBAR_MAX_WIDTH = 420;
const SIDEBAR_COLLAPSE_WIDTH = 80;

function initialSidebarWidth() {
  if (typeof localStorage === "undefined") return SIDEBAR_DEFAULT_WIDTH;
  const raw = localStorage.getItem("vr.sidebar.width");
  if (raw == null) return SIDEBAR_DEFAULT_WIDTH;
  const stored = Number(raw);
  if (!Number.isFinite(stored)) return SIDEBAR_DEFAULT_WIDTH;
  return Math.max(SIDEBAR_MIN_WIDTH, Math.min(SIDEBAR_MAX_WIDTH, stored));
}

// Memoized: App re-renders on every keystroke (edits state), but the sidebar's inputs don't change
// on a keystroke, so it must skip that re-render instead of reconciling the whole explorer tree.
export const Sidebar = memo(SidebarImpl);

function SidebarImpl(props: SidebarProps) {
  const { onCollapsedChange } = props;
  const widthDrag = useRef<{
    pointerId: number;
    startX: number;
    startWidth: number;
    lastWidth?: number;
  } | null>(null);
  const [sidebarWidth, setSidebarWidth] = useState(initialSidebarWidth);
  // Manual open/close only — no hover auto-expand. Collapsed = hidden entirely (no rail).
  const [open, setOpen] = useState(() => localStorage.getItem("vr.sidebar.pinned") !== "0");
  const compact = useMediaQuery("(max-width: 640px)");
  const collapsed = !open || compact;
  useEffect(() => {
    localStorage.setItem("vr.sidebar.pinned", open ? "1" : "0");
  }, [open]);

  // Ctrl+E (App dispatches this) toggles the sidebar from anywhere in the app.
  useEffect(() => {
    const toggle = () => setOpen((o) => !o);
    window.addEventListener(documentEvent("toggle-sidebar"), toggle);
    return () => window.removeEventListener(documentEvent("toggle-sidebar"), toggle);
  }, []);

  useEffect(() => {
    onCollapsedChange?.(collapsed);
  }, [collapsed, onCollapsedChange]);

  // Hydrate the width from the explorer settings mirror only when localStorage has nothing —
  // localStorage always wins so a migrated/reset profile can't stomp the fast path.
  useEffect(() => {
    let live = true;
    void loadExplorerPersisted().then((persisted) => {
      if (!live) return;
      if (localStorage.getItem("vr.sidebar.width") != null) return;
      if (persisted.width >= SIDEBAR_MIN_WIDTH && persisted.width <= SIDEBAR_MAX_WIDTH) {
        setSidebarWidth(persisted.width);
      }
    });
    return () => {
      live = false;
    };
  }, []);

  const startWidthResize = (event: ReactPointerEvent<HTMLDivElement>) => {
    event.preventDefault();
    event.currentTarget.setPointerCapture?.(event.pointerId);
    widthDrag.current = {
      pointerId: event.pointerId,
      startX: event.clientX,
      startWidth: sidebarWidth,
    };
    document.body.style.cursor = "col-resize";
    document.body.style.userSelect = "none";
  };

  useEffect(() => {
    const onMove = (event: PointerEvent) => {
      const drag = widthDrag.current;
      if (!drag || event.pointerId !== drag.pointerId) return;
      const raw = drag.startWidth + event.clientX - drag.startX;
      if (raw <= SIDEBAR_COLLAPSE_WIDTH) {
        setOpen(false);
        return;
      }
      const next = Math.max(SIDEBAR_MIN_WIDTH, Math.min(SIDEBAR_MAX_WIDTH, raw));
      setSidebarWidth(next);
      drag.lastWidth = next;
    };
    const finish = (event: PointerEvent) => {
      const drag = widthDrag.current;
      if (!drag || event.pointerId !== drag.pointerId) return;
      widthDrag.current = null;
      if (drag.lastWidth !== undefined) {
        const width = Math.round(drag.lastWidth);
        localStorage.setItem("vr.sidebar.width", String(width));
        saveExplorerPersisted({ width });
      }
      document.body.style.cursor = "";
      document.body.style.userSelect = "";
    };
    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", finish);
    window.addEventListener("pointercancel", finish);
    return () => {
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", finish);
      window.removeEventListener("pointercancel", finish);
      if (widthDrag.current) {
        document.body.style.cursor = "";
        document.body.style.userSelect = "";
      }
      widthDrag.current = null;
    };
  }, []);

  // Collapsed = the thin rail (narrow/compact windows hide the sidebar entirely).
  if (collapsed) return compact ? null : <SidebarRail {...props} setOpen={setOpen} />;

  return (
    <SidebarExpandedPanel
      {...props}
      sidebarWidth={sidebarWidth}
      startWidthResize={startWidthResize}
      setOpen={setOpen}
      SIDEBAR_MIN_WIDTH={SIDEBAR_MIN_WIDTH}
      SIDEBAR_MAX_WIDTH={SIDEBAR_MAX_WIDTH}
    />
  );
}
