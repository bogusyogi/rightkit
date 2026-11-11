import { useCallback, useMemo } from "react";
import type { TabStripProps } from "./tabs/TabStrip.js";
import { isLocalFileDoc } from "./sidebar/SidebarPathUtils.js";
import type { LoadedDocument } from "@rightkit/document-core/documents/types";
import { documentEvent } from "@rightkit/document-core/host";

type DocsApi = {
  activate: (path: string) => unknown;
  open: (path: string, opts?: { background?: boolean }) => unknown;
  requestClose: TabStripProps["onRequestClose"];
  closeAll: TabStripProps["onCloseAll"];
  closeMany: TabStripProps["onClosePaths"];
  newDocument: (format: "txt") => unknown;
  isDirty: TabStripProps["isDirty"];
};

/** Tab-strip props + tab actions (reorder, bulk close, explorer open, cycle/jump), extracted from
 * App.tsx. Tab order == openDocs order. */
export function useTabChrome({
  tabs,
  docs,
  activePath,
  sidebarCollapsed,
  tabOverviewOpen,
  setTabOverviewOpen,
  pinnedPaths,
  onTogglePin,
  titleBarTitleFor,
  dirtyPaths,
  subscribeDirty,
}: {
  tabs: {
    openDocs: LoadedDocument[];
    setOpenDocs: React.Dispatch<React.SetStateAction<LoadedDocument[]>>;
    subscribeEdits: TabStripProps["subscribeEdits"];
    getEditSnapshot: TabStripProps["getEditSnapshot"];
  };
  docs: DocsApi;
  activePath: string | null;
  sidebarCollapsed: boolean;
  tabOverviewOpen: boolean;
  setTabOverviewOpen: (open: boolean) => void;
  pinnedPaths: TabStripProps["pinnedPaths"];
  onTogglePin: (path: string) => unknown;
  titleBarTitleFor: (d: LoadedDocument) => string;
  dirtyPaths: TabStripProps["dirtyPaths"];
  subscribeDirty: TabStripProps["subscribeDirty"];
}) {
  const { openDocs, subscribeEdits, getEditSnapshot } = tabs;
  const { activate, open, requestClose, closeAll, closeMany, newDocument, isDirty } = docs;
  const reorderTabs = useCallback(
    (fromPath: string, toPath: string, position: "before" | "after" = "before") => {
      tabs.setOpenDocs((prev) => {
        const from = prev.findIndex((doc) => doc.path === fromPath);
        const to = prev.findIndex((doc) => doc.path === toPath);
        if (from < 0 || to < 0 || from === to) return prev;
        const next = [...prev];
        const [moved] = next.splice(from, 1);
        const targetAfterRemoval = from < to ? to - 1 : to;
        const insertAt = position === "after" ? targetAfterRemoval + 1 : targetAfterRemoval;
        next.splice(insertAt, 0, moved);
        return next;
      });
    },
    [tabs],
  );
  // Tab-strip bulk closes: the same Save-All/Discard-All chooser Close All uses, driven by closeMany
  // with the chosen subset. Order = openDocs order so "to the right" means spatial, not sorted.
  const closeOthers = useCallback(
    (path: string) => {
      closeMany(openDocs.filter((d) => d.path !== path).map((d) => d.path));
    },
    [closeMany, openDocs],
  );
  const closeToRight = useCallback(
    (path: string) => {
      const at = openDocs.findIndex((d) => d.path === path);
      if (at < 0) return;
      closeMany(openDocs.slice(at + 1).map((d) => d.path));
    },
    [closeMany, openDocs],
  );
  // Explorer row click: the entry's path is already broker-granted by explorer_list_dir, so this is
  // the same open-or-activate flow recents use (minus the token grant step).
  const openExplorerPath = useCallback(
    (path: string) => {
      if (openDocs.some((d) => d.path === path)) {
        void activate(path);
      } else {
        void open(path);
      }
    },
    [openDocs, activate, open],
  );
  // Tab navigation operates on the visible open-tab order; Ctrl+9 maps to the last tab (browser-style).
  const cycleTab = (dir: 1 | -1) => {
    if (openDocs.length < 2) return;
    const i = openDocs.findIndex((d) => d.path === activePath);
    const n = openDocs.length;
    activate(openDocs[((((i < 0 ? 0 : i) + dir) % n) + n) % n].path);
  };
  const jumpToTab = (index: number) => {
    if (!openDocs.length) return;
    const target = index === 8 ? openDocs.length - 1 : index;
    const doc = openDocs[target];
    if (doc) activate(doc.path);
  };
  // Tab-strip props for the titlebar — memoized so the memoized TabStrip skips per-keystroke App
  // re-renders the same way Sidebar/Toolbar do. openDocs order IS the tab order (single source).
  const toggleSidebarEvent = useCallback(
    () => window.dispatchEvent(new CustomEvent(documentEvent("toggle-sidebar"))),
    [],
  );
  const openLocalPaths = useMemo(
    () => openDocs.filter(isLocalFileDoc).map((d) => d.path),
    [openDocs],
  );
  const openPathsSet = useMemo(() => new Set(openDocs.map((d) => d.path)), [openDocs]);
  const tabStripProps: TabStripProps = useMemo(
    () => ({
      docs: openDocs,
      activePath,
      sidebarCollapsed,
      onToggleSidebar: toggleSidebarEvent,
      onActivate: (path: string) => void activate(path),
      onRequestClose: requestClose,
      onCloseAll: closeAll,
      onCloseOthers: closeOthers,
      onCloseRight: closeToRight,
      onClosePaths: closeMany,
      onReorderTabs: reorderTabs,
      onNewDocument: () => newDocument("txt"),
      onTogglePin: (path: string) => void onTogglePin(path),
      pinnedPaths,
      openLocalPaths,
      titleFor: titleBarTitleFor,
      isDirty,
      dirtyPaths,
      subscribeDirty,
      subscribeEdits,
      getEditSnapshot,
      computeTitle: titleBarTitleFor,
      overviewOpen: tabOverviewOpen,
      onOverviewOpenChange: setTabOverviewOpen,
    }),
    [
      openDocs,
      activePath,
      sidebarCollapsed,
      toggleSidebarEvent,
      activate,
      requestClose,
      closeAll,
      closeOthers,
      closeToRight,
      closeMany,
      reorderTabs,
      newDocument,
      onTogglePin,
      pinnedPaths,
      openLocalPaths,
      titleBarTitleFor,
      isDirty,
      dirtyPaths,
      subscribeDirty,
      subscribeEdits,
      getEditSnapshot,
      tabOverviewOpen,
      setTabOverviewOpen,
    ],
  );
  return { tabStripProps, openPathsSet, openExplorerPath, cycleTab, jumpToTab };
}
