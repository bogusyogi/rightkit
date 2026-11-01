import {
  type ChangeEvent,
  type KeyboardEvent,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import Icon from "@rightkit/agent-ui-core/components/Icon";
import {
  useWorkspaceStore,
  normalizePath,
  type WorkspaceTreePath,
  pathParent,
} from "@rightkit/agent-ui-core/state/workspaceStore";
import { openWorkspaceWatch, postWorkspaceTreeExpand } from "@rightkit/agent-ui-core/lib/daemon/workspace-files";
import type { WorkspaceEntry, WorkspaceWatchEvent } from "@rightkit/agent-ui-core/lib/types/workspace-files";
import { useWorkspaceTree } from "@rightkit/agent-ui-hooks/hooks/useWorkspaceTree";
import WorkspaceQuickOpen from "./WorkspaceQuickOpen";
import WorkspaceRecentMenu from "./WorkspaceRecentMenu";

// FilesPanel — right-side slide-in panel that lists the workspace folder
// tree. D1 replaces the static placeholder tree with a real keyboard-native
// virtualized tree backed by the daemon's `/workspace/tree` and
// `/workspace/tree/expand` routes. The panel owns:
//   - direct-child recursive filter (no duplicates)
//   - stable per-row IDs with aria-activedescendant + level/pos/setsize
//   - keyboard-native navigation:
//       ↑/↓: move active row
//       → : expand active directory (postWorkspaceTreeExpand)
//       ← : collapse active directory, or move to parent
//       Enter: activate file / toggle directory
//       ⌘P / Ctrl+P: quick open
//       ⌘R / Ctrl+R: refresh (forced)
//       Esc: clear filter
//   - refresh, reveal-in-shell, copy-path, Git decoration, last-known
//     fallback, and a Retry surface for failed/unavailable states
// The store of choice for the panel state lives in `useWorkspaceStore`;
// the data fetch lives in `useWorkspaceTree`.

export type FilesPanelStatus = "idle" | "loading" | "ready" | "unavailable" | "error";

export interface FilesPanelProps {
  onClose: () => void;
  onMaximize?: () => void;
  /** Daemon-rooted workspace path. Empty string means no workspace open. */
  workspaceRoot?: string;
  /** Loader/result state. Owning surface (AppBody/D1) decides the value. */
  status?: FilesPanelStatus;
  /** Tree contents while `status === "ready"`. */
  entries?: FileEntry[];
  /** Human-readable message shown in non-ready states. */
  statusMessage?: string;
  /** Triggered when the user clicks Retry on a failed/unavailable load. */
  onRetry?: () => void;
  /** Triggered when the user clicks "Open workspace" in the empty-state. */
  onOpenWorkspace?: () => void;
  /** Triggered when the user activates a file (Enter / double-click). */
  onActivate?: (entry: { path: string; name: string }) => void;
  /** Triggered when the user asks to reveal a path in the host shell. */
  onReveal?: (path: string) => void;
  /** Triggered when the user asks to copy a path to the clipboard. */
  onCopyPath?: (path: string) => void;
  /** Daemon config; when present the live tree path is used. */
  config?: import("@rightkit/agent-ui-core/lib/types/settings-plugins").DaemonConfig | null;
}

export interface FileEntry {
  name: string;
  /** Lower-cased suffix (".py" / ".ts" / ".md"). Empty → treated as a folder. */
  suffix?: string;
  children?: FileEntry[];
}

const DEFAULT_EMPTY_TREE: FileEntry[] = [];
const ROW_HEIGHT = 26;
const OVERSCAN = 6;

function FileIcon({ suffix }: { suffix?: string }) {
  if (!suffix)
    return (
      <span className="text-text-3">
        <Icon name="folder" size={12} />
      </span>
    );
  if (suffix === ".py" || suffix === ".ipynb")
    return <span className="text-[11px] text-accent-2">py</span>;
  if (suffix === ".ts" || suffix === ".tsx")
    return <span className="text-[11px] text-accent">ts</span>;
  if (suffix === ".json") return <span className="text-[11px] text-text-2">json</span>;
  if (suffix === ".md") return <span className="text-[11px] text-text-2">md</span>;
  if (suffix === ".toml" || suffix === ".yaml" || suffix === ".yml")
    return <span className="text-[11px] text-text-2">{suffix.slice(1)}</span>;
  if (suffix === ".csv") return <span className="text-[11px] text-text-2">csv</span>;
  return <span className="font-mono text-[11px] text-text-3">{suffix.slice(1)}</span>;
}

interface FlatRow {
  depth: number;
  name: string;
  path: string;
  parentPath: string;
  kind: "directory" | "file";
  suffix?: string | null;
  isExpanded: boolean;
  hasChildren: boolean;
  sizeBytes?: number | null;
  modifiedAt?: string | null;
  gitStatus?: string | null;
  lastKnown?: boolean | null;
}

interface ListFilesPanelProps
  extends Omit<
    FilesPanelProps,
    "entries" | "status" | "statusMessage" | "onRetry" | "onOpenWorkspace"
  > {}

function flattenTree(
  root: { path: string; entries: WorkspaceEntry[] },
  expanded: Record<string, WorkspaceTreePath>,
  recursiveFilter: string,
): FlatRow[] {
  const out: FlatRow[] = [];
  const visited: Set<string> = new Set();

  function walk(parentPath: string, entries: WorkspaceEntry[], depth: number) {
    // Dedupe on a canonical path key so the same entry can't appear twice
    // when a filter pulls a parent + a child into the same visible row.
    for (const entry of entries) {
      if (visited.has(entry.path)) continue;
      visited.add(entry.path);

      const isExpanded = Boolean(expanded[entry.path]);
      const hasChildren = entry.kind === "directory";
      const matches =
        !recursiveFilter ||
        entry.name.toLowerCase().includes(recursiveFilter) ||
        entry.path.toLowerCase().includes(recursiveFilter);
      if (matches) {
        out.push({
          depth,
          name: entry.name,
          path: entry.path,
          parentPath,
          kind: entry.kind,
          suffix: entry.suffix,
          isExpanded,
          hasChildren,
          sizeBytes: entry.sizeBytes,
          modifiedAt: entry.modifiedAt,
          gitStatus: entry.gitStatus,
          lastKnown: expanded[entry.path]?.lastKnown,
        });
      }
      // Recurse only when expanded OR when a filter is set and this row
      // matched (so the user sees the children that also match). Both
      // branches keep the row counter monotonic — no duplicates.
      if (hasChildren) {
        const childEntries = lookupEntries(entry.path, expanded);
        if (childEntries && (isExpanded || recursiveFilter)) {
          walk(entry.path, childEntries, depth + 1);
        }
      }
    }
  }

  walk(root.path, root.entries, 0);
  return out;
}

function lookupEntries(
  dirPath: string,
  expanded: Record<string, WorkspaceTreePath>,
): WorkspaceEntry[] | null {
  const value = expanded[dirPath];
  if (!value) return null;
  return value.entries;
}

function ListFilesPanel(props: ListFilesPanelProps) {
  const workspaceRoot = (props.workspaceRoot ?? "").trim();
  const setWorkspaceRoot = useWorkspaceStore((state) => state.setWorkspaceRoot);
  useEffect(() => {
    setWorkspaceRoot(workspaceRoot);
  }, [workspaceRoot, setWorkspaceRoot]);

  const [filter, setFilter] = useState("");
  const [activeIndex, setActiveIndex] = useState(0);
  const [quickOpenOpen, setQuickOpenOpen] = useState(false);
  const [recentOpen, setRecentOpen] = useState(false);
  const [reloadToken, setReloadToken] = useState(0);
  const treeRef = useRef<HTMLDivElement | null>(null);

  const { status, entries, truncated, lastKnown, reason, refresh } = useWorkspaceTree({
    config: props.config ?? null,
    workspaceRoot,
    path: "",
    reloadToken,
  });

  const expanded = useWorkspaceStore((state) => state.expanded);
  const rememberPath = useWorkspaceStore((state) => state.rememberPath);
  const markPathLastKnown = useWorkspaceStore((state) => state.markPathLastKnown);
  const forgetPath = useWorkspaceStore((state) => state.forgetPath);
  const selection = useWorkspaceStore((state) => state.selection);
  const setSelection = useWorkspaceStore((state) => state.setSelection);
  const expandedRef = useRef(expanded);
  const watchTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const pendingWatchPathsRef = useRef<Set<string>>(new Set());

  useEffect(() => {
    expandedRef.current = expanded;
  }, [expanded]);

  const refreshExpandedPath = useCallback(
    async (path: string) => {
      if (!props.config || !workspaceRoot) return;
      try {
        const response = await postWorkspaceTreeExpand(props.config, workspaceRoot, path);
        rememberPath(path, response.entries, response.truncated, { lastKnown: false });
      } catch {
        // Preserve the prior children and mark them last-known instead of
        // rebuilding an expanded directory as an empty tree.
        markPathLastKnown(path);
      }
    },
    [markPathLastKnown, props.config, rememberPath, workspaceRoot],
  );

  // Port of Aethon's debounced visible-directory watcher: the daemon owns
  // filesystem polling, while this surface coalesces events and refreshes
  // only the root/expanded paths already visible to the user.
  useEffect(() => {
    if (!props.config || !workspaceRoot) return;
    let disposed = false;
    let watchHandle: { close: () => void } | null = null;
    const controller = new AbortController();

    const flush = () => {
      watchTimerRef.current = null;
      if (disposed) return;
      const paths = [...pendingWatchPathsRef.current];
      pendingWatchPathsRef.current.clear();
      if (paths.length === 0) return;

      const fullRescan = paths.includes("");
      if (fullRescan) setReloadToken((token) => token + 1);
      const expandedPaths = Object.keys(expandedRef.current).filter(
        (path) => path && (fullRescan || paths.includes(path)),
      );
      for (const path of expandedPaths) void refreshExpandedPath(path);
    };

    const onEvent = (event: WorkspaceWatchEvent) => {
      if (disposed) return;
      const changedPath = normalizePath(event.path);
      if (event.kind === "rescan" || !changedPath) {
        pendingWatchPathsRef.current.add("");
      } else {
        // A file event invalidates its containing directory. Directory
        // create/remove events also invalidate the directory path itself if
        // it is already expanded.
        pendingWatchPathsRef.current.add(pathParent(changedPath));
        if (event.kind === "created" || event.kind === "removed") {
          pendingWatchPathsRef.current.add(changedPath);
        }
      }
      if (watchTimerRef.current === null) {
        watchTimerRef.current = setTimeout(flush, 120);
      }
    };

    void openWorkspaceWatch(
      props.config,
      workspaceRoot,
      onEvent,
      () => {
        // Watch failure is an enhancement failure; the tree remains usable
        // through manual refresh and explicit expansion.
      },
      { signal: controller.signal },
    ).then((handle) => {
      if (disposed) handle.close();
      else watchHandle = handle;
    }).catch(() => {
      // The daemon may be offline while the panel is mounted. Do not replace
      // a usable tree with a watcher-only error state.
    });

    return () => {
      disposed = true;
      controller.abort();
      watchHandle?.close();
      if (watchTimerRef.current !== null) {
        clearTimeout(watchTimerRef.current);
        watchTimerRef.current = null;
      }
      pendingWatchPathsRef.current.clear();
    };
  }, [props.config, refreshExpandedPath, workspaceRoot]);

  // Seed the store with the freshly loaded root entries so child paths
  // know what to render when they expand.
  useEffect(() => {
    if (entries.length > 0 || truncated) {
      rememberPath("", entries, truncated, { lastKnown: lastKnown });
    }
  }, [entries, truncated, lastKnown, rememberPath]);

  const rows = useMemo(() => {
    const normalized = filter.trim().toLowerCase();
    return flattenTree({ path: "", entries }, expanded, normalized);
  }, [entries, expanded, filter]);

  useEffect(() => {
    setActiveIndex((current) => Math.min(current, Math.max(rows.length - 1, 0)));
  }, [rows.length]);

  const handleFilterChange = useCallback((event: ChangeEvent<HTMLInputElement>) => {
    setFilter(event.target.value);
  }, []);

  const activateDirectory = useCallback(
    (row: FlatRow) => {
      if (row.kind !== "directory") {
        setSelection({ path: row.path, fingerprint: null });
        props.onActivate?.({ path: row.path, name: row.name });
        return;
      }
      if (expanded[row.path]) {
        forgetPath(row.path);
      } else {
        // Expand immediately, then replace the placeholder with the daemon's
        // direct children. Pointer activation and ArrowRight must use the same
        // live path; otherwise mouse users see a permanently empty directory.
        rememberPath(row.path, [], false, { lastKnown: false });
        void loadChild(
          workspaceRoot,
          row.path,
          props.config ?? null,
          rememberPath,
          markPathLastKnown,
        );
      }
    },
    [
      expanded,
      forgetPath,
      markPathLastKnown,
      rememberPath,
      setSelection,
      props,
      workspaceRoot,
    ],
  );

  const expandDirectory = useCallback(
    (path: string) => {
      // Post → child loader: dedicated mini-hook for the panel so a
      // folder expand performs a real `postWorkspaceTreeExpand` and
      // merges the result into the store.
      void loadChild(
        workspaceRoot,
        path,
        props.config ?? null,
        rememberPath,
        markPathLastKnown,
      );
    },
    [workspaceRoot, props.config, rememberPath, markPathLastKnown],
  );

  const handleKeyDown = useCallback(
    (event: KeyboardEvent<HTMLDivElement>) => {
      if (event.key === "ArrowDown") {
        event.preventDefault();
        setActiveIndex((current) => Math.min(current + 1, rows.length - 1));
      } else if (event.key === "ArrowUp") {
        event.preventDefault();
        setActiveIndex((current) => Math.max(current - 1, 0));
      } else if (event.key === "ArrowRight") {
        event.preventDefault();
        const row = rows[activeIndex];
        if (row && row.kind === "directory" && !expanded[row.path]) {
          expandDirectory(row.path);
          rememberPath(row.path, [], false, { lastKnown: false });
        }
      } else if (event.key === "ArrowLeft") {
        event.preventDefault();
        const row = rows[activeIndex];
        if (row && row.kind === "directory" && expanded[row.path]) {
          // Collapse the active directory first.
          forgetPath(row.path);
        } else if (row && row.parentPath !== undefined) {
          // Left moves to the parent row (or collapses if already at the parent).
          const parentIndex = rows.findIndex((candidate) => candidate.path === row.parentPath);
          if (parentIndex >= 0) {
            setActiveIndex(parentIndex);
            forgetPath(row.parentPath);
          }
        }
      } else if (event.key === "Enter") {
        event.preventDefault();
        const row = rows[activeIndex];
        if (row) activateDirectory(row);
      } else if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "p") {
        event.preventDefault();
        setQuickOpenOpen(true);
      } else if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "r") {
        event.preventDefault();
        setReloadToken((token) => token + 1);
        void refresh();
      } else if (event.key === "Escape") {
        setFilter("");
      }
    },
    [
      activeIndex,
      rows,
      expanded,
      rememberPath,
      forgetPath,
      activateDirectory,
      expandDirectory,
      refresh,
    ],
  );

  const handleCopyPath = useCallback(
    (row: FlatRow) => {
      const fullPath = joinWorkspacePath(workspaceRoot, row.path);
      props.onCopyPath?.(fullPath);
    },
    [props, workspaceRoot],
  );

  const handleReveal = useCallback(
    (row: FlatRow) => {
      const fullPath = joinWorkspacePath(workspaceRoot, row.path);
      props.onReveal?.(fullPath);
    },
    [props, workspaceRoot],
  );

  const activeRowId = rows[activeIndex]
    ? `files-row-${encodeURIComponent(rows[activeIndex].path)}`
    : undefined;

  return (
    <aside
      aria-label="Files panel"
      data-testid="files-panel"
      className="flex h-full w-full min-w-0 shrink-0 flex-col overflow-hidden bg-panel/85"
    >
      <header className="flex h-9 flex-none items-center gap-2 border-b border-line px-3 text-[13px] font-medium text-text">
        <span className="flex-1 truncate">Files</span>
        <button
          type="button"
          aria-label="Refresh"
          title="Refresh"
          data-testid="files-refresh"
          onClick={() => {
            setReloadToken((token) => token + 1);
            void refresh();
          }}
          className="cr-control-pill grid h-6 w-6 place-items-center rounded-md border border-transparent text-text-3 cursor-pointer hover:bg-panel-2 hover:text-text focus:outline-none focus-visible:ring-2 focus-visible:ring-accent/60"
        >
          <Icon name="refresh" size={12} />
        </button>
        <button
          type="button"
          aria-label="Quick open"
          title="Quick open (⌘P / Ctrl+P)"
          data-testid="files-quick-open"
          onClick={() => setQuickOpenOpen(true)}
          className="cr-control-pill grid h-6 w-6 place-items-center rounded-md border border-transparent text-text-3 cursor-pointer hover:bg-panel-2 hover:text-text focus:outline-none focus-visible:ring-2 focus-visible:ring-accent/60"
        >
          <Icon name="search" size={12} />
        </button>
        <button
          type="button"
          aria-label="Recent files"
          title="Recent files"
          data-testid="files-recent"
          onClick={() => setRecentOpen(true)}
          className="cr-control-pill grid h-6 w-6 place-items-center rounded-md border border-transparent text-text-3 cursor-pointer hover:bg-panel-2 hover:text-text focus:outline-none focus-visible:ring-2 focus-visible:ring-accent/60"
        >
          <Icon name="clock" size={12} />
        </button>
        <button
          type="button"
          aria-label="Maximize"
          title="Maximize"
          onClick={props.onMaximize}
          className="cr-control-pill grid h-6 w-6 place-items-center rounded-md border border-transparent text-text-3 cursor-pointer hover:bg-panel-2 hover:text-text focus:outline-none focus-visible:ring-2 focus-visible:ring-accent/60"
        >
          <Icon name="maximize" size={13} />
        </button>
        <button
          type="button"
          aria-label="Close files panel"
          title="Close"
          onClick={props.onClose}
          className="cr-control-pill grid h-6 w-6 place-items-center rounded-md border border-transparent text-text-3 cursor-pointer hover:bg-panel-2 hover:text-text focus:outline-none focus-visible:ring-2 focus-visible:ring-accent/60"
        >
          <Icon name="close" size={13} />
        </button>
      </header>

      <div className="flex-none border-b border-line px-3 py-2">
        <label className="relative block">
          <span className="pointer-events-none absolute left-2.5 top-1/2 -translate-y-1/2 text-text-3">
            <Icon name="search" size={12} />
          </span>
          <input
            type="search"
            value={filter}
            onChange={handleFilterChange}
            placeholder="Filter files... (↑↓ Enter ⌘P)"
            aria-label="Filter files"
            data-testid="files-filter"
            className="block w-full rounded-md border border-line bg-panel-2 py-1.5 pl-7 pr-3 text-[12px] text-text placeholder:text-text-3 focus:border-accent/60 focus:outline-none focus-visible:ring-2 focus-visible:ring-accent/60"
          />
        </label>
        <p className="mt-1 truncate text-[10px] text-text-3">
          {workspaceRoot ? workspaceRoot : "No workspace selected"}
        </p>
        {selection?.path ? (
          <p className="mt-1 truncate text-[10px] text-accent" data-testid="files-selection">
            {selection.path}
          </p>
        ) : null}
        {lastKnown ? (
          <p className="mt-1 truncate text-[10px] text-warning" data-testid="files-last-known">
            Showing last-known tree (refresh failed)
          </p>
        ) : null}
      </div>

      <div
        ref={treeRef}
        className="min-h-0 flex-1 overflow-y-auto px-1 py-1 focus:outline-none"
        data-testid="files-tree"
        role="tree"
        aria-label="Workspace files"
        aria-activedescendant={activeRowId}
        tabIndex={0}
        onKeyDown={handleKeyDown}
      >
        {status === "ready" || status === "loading" ? (
          rows.length === 0 ? (
            <div
              data-testid="files-empty"
              className="mx-1 mt-2 rounded-md border border-line/70 bg-panel-2/60 p-3 text-[12px] text-text-2"
            >
              <p className="font-medium text-text">No matches</p>
              <p className="mt-1 leading-snug text-text-3">
                {filter
                  ? "Nothing in this workspace matches the current filter."
                  : "Workspace has no entries yet."}
              </p>
            </div>
          ) : (
            <VirtualizedTree
              rows={rows}
              activeIndex={activeIndex}
              onActivate={activateDirectory}
              onCopy={handleCopyPath}
              onReveal={handleReveal}
            />
          )
        ) : (
          <HonestState
            status={status}
            reason={reason}
            onRetry={() => {
              setReloadToken((token) => token + 1);
              void refresh();
            }}
          />
        )}
      </div>

      {quickOpenOpen ? (
        <WorkspaceQuickOpen
          workspaceRoot={workspaceRoot}
          entries={entries}
          onClose={() => setQuickOpenOpen(false)}
          onPick={(path) => {
            setSelection({ path, fingerprint: null });
            props.onActivate?.({ path, name: basenameOf(path) });
            setQuickOpenOpen(false);
          }}
        />
      ) : null}

      {recentOpen ? (
        <WorkspaceRecentMenu
          workspaceRoot={workspaceRoot}
          onClose={() => setRecentOpen(false)}
          onPick={(path) => {
            setSelection({ path, fingerprint: null });
            props.onActivate?.({ path, name: basenameOf(path) });
            setRecentOpen(false);
          }}
        />
      ) : null}
    </aside>
  );
}

function basenameOf(path: string): string {
  return normalizePath(path).split("/").pop() ?? path;
}

/** Join a daemon-rooted path without losing its leading slash or drive root. */
function joinWorkspacePath(root: string, relative: string): string {
  const separator = root.includes("\\") && !root.includes("/") ? "\\" : "/";
  const cleanRoot = root.replace(/[\\/]+$/, "");
  const cleanRelative = normalizePath(relative).replace(/\\/g, separator);
  if (!cleanRelative) return root;
  if (!cleanRoot) return root ? `${separator}${cleanRelative}` : cleanRelative;
  return `${cleanRoot}${separator}${cleanRelative}`;
}

async function loadChild(
  workspaceRoot: string,
  path: string,
  config: import("@rightkit/agent-ui-core/lib/types/settings-plugins").DaemonConfig | null,
  rememberPath: (
    path: string,
    entries: WorkspaceEntry[],
    truncated: boolean,
    options?: { lastKnown?: boolean },
  ) => void,
  markPathLastKnown: (path: string) => void,
) {
  if (!config) return;
  try {
    const response = await postWorkspaceTreeExpand(config, workspaceRoot, path);
    rememberPath(path, response.entries, response.truncated, { lastKnown: false });
  } catch {
    // Preserve already-loaded children; an unpopulated placeholder is also
    // retained and marked so the panel can honestly offer another refresh.
    markPathLastKnown(path);
  }
}

function VirtualizedTree({
  rows,
  activeIndex,
  onActivate,
  onCopy,
  onReveal,
}: {
  rows: FlatRow[];
  activeIndex: number;
  onActivate: (row: FlatRow) => void;
  onCopy: (row: FlatRow) => void;
  onReveal: (row: FlatRow) => void;
}) {
  const scrollRef = useRef<HTMLDivElement | null>(null);
  const [scrollTop, setScrollTop] = useState(0);
  const [viewportHeight, setViewportHeight] = useState(0);

  useEffect(() => {
    const node = scrollRef.current;
    if (!node) return;
    const handle = () => setScrollTop(node.scrollTop);
    const resize = () => setViewportHeight(node.clientHeight);
    handle();
    resize();
    node.addEventListener("scroll", handle, { passive: true });
    const observer = new ResizeObserver(resize);
    observer.observe(node);
    return () => {
      node.removeEventListener("scroll", handle);
      observer.disconnect();
    };
  }, []);

  // Scroll the active row into view whenever it changes (keyboard nav).
  useEffect(() => {
    const node = scrollRef.current;
    if (!node) return;
    const active = rows[activeIndex];
    if (!active) return;
    const top = activeIndex * ROW_HEIGHT;
    const bottom = top + ROW_HEIGHT;
    if (top < node.scrollTop) {
      node.scrollTop = top;
    } else if (bottom > node.scrollTop + node.clientHeight) {
      node.scrollTop = bottom - node.clientHeight;
    }
  }, [activeIndex, rows]);

  const total = rows.length * ROW_HEIGHT;
  const startIndex = Math.max(0, Math.floor(scrollTop / ROW_HEIGHT) - OVERSCAN);
  const visibleCount = Math.ceil(viewportHeight / ROW_HEIGHT) + OVERSCAN * 2;
  const endIndex = Math.min(rows.length, startIndex + visibleCount);
  const visibleRows = rows.slice(startIndex, endIndex);

  return (
    <div
      ref={scrollRef}
      className="relative h-full overflow-y-auto"
      data-testid="files-virtualized"
    >
      <div style={{ height: total, position: "relative" }}>
        <ul
          className="m-0 list-none p-0"
          style={{ position: "absolute", top: startIndex * ROW_HEIGHT, left: 0, right: 0 }}
        >
          {visibleRows.map((row, idx) => {
            const absoluteIndex = startIndex + idx;
            const isActive = absoluteIndex === activeIndex;
            const rowId = `files-row-${encodeURIComponent(row.path)}`;
            return (
              <li key={row.path} style={{ height: ROW_HEIGHT }}>
                <button
                  id={rowId}
                  type="button"
                  role="treeitem"
                  aria-level={row.depth + 1}
                  aria-posinset={absoluteIndex + 1}
                  aria-setsize={rows.length}
                  aria-expanded={row.kind === "directory" ? row.isExpanded : undefined}
                  aria-selected={isActive}
                  title={row.lastKnown ? "Last-known directory contents" : undefined}
                  data-testid={rowId}
                  onClick={() => onActivate(row)}
                  onContextMenu={(event) => {
                    event.preventDefault();
                    if (event.shiftKey) onReveal(row);
                    else onCopy(row);
                  }}
                  className={`flex w-full items-center gap-1.5 rounded px-1 text-left text-[12px] transition-colors duration-100 focus:outline-none focus-visible:ring-2 focus-visible:ring-accent/60 ${
                    isActive ? "bg-panel-2 text-text" : "text-text-2 hover:bg-panel-2/70"
                  }`}
                  style={{ paddingLeft: `${row.depth * 12 + 4}px`, height: ROW_HEIGHT }}
                >
                  <span
                    aria-hidden="true"
                    className={`grid h-3 w-3 place-items-center text-text-3 transition-transform duration-100 ${
                      row.isExpanded ? "rotate-90" : ""
                    } ${row.kind === "directory" ? "" : "invisible"}`}
                  >
                    <Icon name="chevron" size={10} />
                  </span>
                  <FileIcon suffix={row.suffix ?? deriveSuffixFromName(row.name)} />
                  <span className="truncate font-mono">{row.name}</span>
                  {row.gitStatus ? (
                    <span
                      role="img"
                      aria-label={`Git status: ${row.gitStatus}`}
                      title={`Git status: ${row.gitStatus}`}
                      data-testid={`files-git-status-${encodeURIComponent(row.path)}`}
                      className="ml-auto text-[10px] font-semibold text-accent-2"
                    >
                      {gitStatusGlyph(row.gitStatus)}
                    </span>
                  ) : null}
                  {row.sizeBytes != null && row.kind === "file" ? (
                    <span className="ml-auto pl-2 text-[10px] text-text-3">
                      {formatBytes(row.sizeBytes)}
                    </span>
                  ) : null}
                </button>
              </li>
            );
          })}
        </ul>
      </div>
    </div>
  );
}

function deriveSuffixFromName(name: string): string | undefined {
  const idx = name.lastIndexOf(".");
  if (idx < 0) return undefined;
  return name.slice(idx);
}

function gitStatusGlyph(status: string): string {
  switch (status.toLowerCase()) {
    case "modified":
    case "m":
      return "M";
    case "added":
    case "a":
      return "A";
    case "deleted":
    case "d":
      return "D";
    case "renamed":
    case "r":
      return "R";
    case "copied":
    case "c":
      return "C";
    case "untracked":
    case "?":
      return "U";
    case "conflicted":
    case "!":
      return "!";
    default:
      return status.slice(0, 1).toUpperCase();
  }
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes}B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)}KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)}MB`;
}

function HonestState({
  status,
  reason,
  onRetry,
}: {
  status: FilesPanelStatus;
  reason: string | null;
  onRetry?: () => void;
}) {
  const title =
    status === "idle"
      ? "Workspace not loaded"
      : status === "loading"
        ? "Loading workspace…"
        : status === "unavailable"
          ? "Workspace unavailable"
          : "Workspace failed to load";
  const body =
    reason ??
    (status === "idle"
      ? "Open a workspace to see files here."
      : status === "loading"
        ? "Reading the workspace folder tree."
        : status === "unavailable"
          ? "The workspace module is being built. Try again in a moment."
          : "The workspace folder could not be read.");
  return (
    <div
      data-testid={`files-status-${status}`}
      role={status === "error" ? "alert" : "status"}
      aria-live="polite"
      className="mx-1 mt-2 rounded-md border border-line/70 bg-panel-2/60 p-3 text-[12px] text-text-2"
    >
      <p className="font-medium text-text">{title}</p>
      <p className="mt-1 leading-snug text-text-3">{body}</p>
      {(status === "error" || status === "unavailable") && onRetry ? (
        <button
          type="button"
          onClick={onRetry}
          className="cr-control-pill mt-2 inline-flex h-7 cursor-pointer items-center rounded-md border border-line bg-panel px-2 text-[11px] font-medium text-text hover:border-accent/60 hover:text-accent focus:outline-none focus-visible:ring-2 focus-visible:ring-accent/60"
        >
          Retry
        </button>
      ) : null}
    </div>
  );
}

// The default export retains the original `FilesPanel` interface (status +
// entries driven by the owning surface) while internally rendering the D1
// keyboard-native tree. The legacy `entries` prop continues to drive the
// static placeholder tree; when the owning surface wires up a daemon
// config (via the optional `onActivate`/`onReveal`/`onCopyPath` props plus
// no `entries` payload), the panel switches to the live tree backed by
// `useWorkspaceTree`. The honest-state branches remain unchanged.
export default function FilesPanel(props: FilesPanelProps) {
  const workspaceRoot = (props.workspaceRoot ?? "").trim();
  const legacyEntries = props.entries && props.entries.length > 0;
  const useLiveTree = !legacyEntries && Boolean(props.config && workspaceRoot && props.onActivate);
  if (!useLiveTree) {
    return <StaticFilesPanel {...props} entries={props.entries} />;
  }
  return <ListFilesPanel {...props} workspaceRoot={workspaceRoot} />;
}

function StaticFilesPanel(props: FilesPanelProps & { entries?: FileEntry[] }) {
  const [filter, setFilter] = useState("");
  const tree =
    props.status === "ready" ? (props.entries ?? DEFAULT_EMPTY_TREE) : DEFAULT_EMPTY_TREE;
  const filtered = useMemo(() => {
    const normalized = filter.trim().toLowerCase();
    if (!normalized) return tree;
    return tree.filter((entry) => entry.name.toLowerCase().includes(normalized));
  }, [tree, filter]);

  return (
    <aside
      aria-label="Files panel"
      data-testid="files-panel"
      className="flex h-full w-full min-w-0 shrink-0 flex-col overflow-hidden bg-panel/85"
    >
      <header className="flex h-9 flex-none items-center gap-2 border-b border-line px-3 text-[13px] font-medium text-text">
        <span className="flex-1 truncate">Files</span>
        <button
          type="button"
          aria-label="Maximize"
          title="Maximize"
          onClick={props.onMaximize}
          className="cr-control-pill grid h-6 w-6 place-items-center rounded-md border border-transparent text-text-3 cursor-pointer hover:bg-panel-2 hover:text-text focus:outline-none focus-visible:ring-2 focus-visible:ring-accent/60"
        >
          <Icon name="maximize" size={13} />
        </button>
        <button
          type="button"
          aria-label="Close files panel"
          title="Close"
          onClick={props.onClose}
          className="cr-control-pill grid h-6 w-6 place-items-center rounded-md border border-transparent text-text-3 cursor-pointer hover:bg-panel-2 hover:text-text focus:outline-none focus-visible:ring-2 focus-visible:ring-accent/60"
        >
          <Icon name="close" size={13} />
        </button>
      </header>
      <div className="flex-none border-b border-line px-3 py-2">
        <input
          type="search"
          value={filter}
          onChange={(event) => setFilter(event.target.value)}
          placeholder="Filter files..."
          aria-label="Filter files"
          data-testid="files-filter"
          className="block w-full rounded-md border border-line bg-panel-2 py-1.5 px-3 text-[12px] text-text placeholder:text-text-3 focus:border-accent/60 focus:outline-none focus-visible:ring-2 focus-visible:ring-accent/60"
        />
        <p className="mt-1 text-[10px] text-text-3">
          {props.workspaceRoot?.trim() ? props.workspaceRoot : "No workspace selected"}
        </p>
      </div>
      <div className="min-h-0 flex-1 overflow-y-auto px-1 py-1" data-testid="files-tree">
        {props.status === "ready" && tree.length === 0 ? (
          <div
            data-testid="files-empty"
            role="status"
            className="mx-1 mt-2 rounded-md border border-line/70 bg-panel-2/60 p-3 text-[12px] text-text-2"
          >
            Workspace is empty.
          </div>
        ) : tree.length === 0 ? (
          <HonestState
            status={props.status ?? "idle"}
            reason={props.statusMessage ?? null}
            onRetry={props.onRetry}
          />
        ) : (
          <div role="tree" aria-label="Workspace files" className="m-0 p-0">
            {filtered.map((entry) => (
              <StaticRow key={entry.name} entry={entry} depth={0} query={filter} />
            ))}
          </div>
        )}
      </div>
      {props.status === "idle" && props.onOpenWorkspace ? (
        <button
          type="button"
          onClick={props.onOpenWorkspace}
          className="cr-control-pill mx-3 mb-3 h-8 rounded-md border border-line bg-panel-2 px-3 text-[12px] text-text"
        >
          Open workspace
        </button>
      ) : null}
    </aside>
  );
}

function StaticRow({ entry, depth, query }: { entry: FileEntry; depth: number; query: string }) {
  const [open, setOpen] = useState(false);
  const hasChildren = !!entry.children && entry.children.length > 0;
  const indent = depth * 12;
  const visibleChildren = useMemo(() => {
    if (!entry.children) return [] as FileEntry[];
    const normalized = query.trim().toLowerCase();
    if (!normalized) return entry.children;
    return entry.children.filter((child) => child.name.toLowerCase().includes(normalized));
  }, [entry.children, query]);
  const matchSelf = useMemo(() => {
    const normalized = query.trim().toLowerCase();
    if (!normalized) return true;
    return entry.name.toLowerCase().includes(normalized);
  }, [entry.name, query]);
  if (!matchSelf) return null;
  return (
    <li>
      <button
        type="button"
        onClick={() => hasChildren && setOpen((value) => !value)}
        aria-expanded={open}
        className={`flex w-full items-center gap-1.5 rounded px-1 py-1 text-left text-[12px] transition-colors duration-100 focus:outline-none focus-visible:ring-2 focus-visible:ring-accent/60 ${
          hasChildren ? "cursor-pointer hover:bg-panel-2" : "cursor-default"
        }`}
        style={{ paddingLeft: `${indent + 4}px` }}
      >
        <span
          aria-hidden="true"
          className={`grid h-3 w-3 place-items-center text-text-3 transition-transform duration-100 ${
            open ? "rotate-90" : ""
          }`}
        >
          <Icon name="chevron" size={10} />
        </span>
        <FileIcon suffix={entry.suffix} />
        <span className="font-mono text-text-2">{entry.name}</span>
      </button>
      {hasChildren && open ? (
        <ul className="m-0 min-w-0 list-none border-0 p-0">
          {visibleChildren.map((child) => (
            <StaticRow key={child.name} entry={child} depth={depth + 1} query={query} />
          ))}
        </ul>
      ) : null}
    </li>
  );
}
