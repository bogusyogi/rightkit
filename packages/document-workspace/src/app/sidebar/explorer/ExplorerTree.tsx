// The explorer directory tree: lazy one-level listing per expanded dir (explorer_list_dir),
// non-recursive watches while a dir is expanded (explorer_watch_dir / explorer_unwatch_dir +
// "explorer:changed" re-lists just that dir), ARIA tree semantics with roving tabindex and
// arrow-key navigation, and the shared row chrome (caret, folder/format icon, name, "…" button).
import {
  memo,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
  type MouseEvent as ReactMouseEvent,
  type ReactNode,
} from "react";
import "../../../styles/explorer.css";
import { CaretDown } from "@phosphor-icons/react/dist/icons/CaretDown";
import { CaretRight } from "@phosphor-icons/react/dist/icons/CaretRight";
import { Cloud } from "@phosphor-icons/react/dist/icons/Cloud";
import { Desktop } from "@phosphor-icons/react/dist/icons/Desktop";
import { DotsThree } from "@phosphor-icons/react/dist/icons/DotsThree";
import { DownloadSimple } from "@phosphor-icons/react/dist/icons/DownloadSimple";
import { File } from "@phosphor-icons/react/dist/icons/File";
import { FileText } from "@phosphor-icons/react/dist/icons/FileText";
import { Folder } from "@phosphor-icons/react/dist/icons/Folder";
import { HardDrives } from "@phosphor-icons/react/dist/icons/HardDrives";
import { House } from "@phosphor-icons/react/dist/icons/House";
import { FileTypeIcon } from "../../../documents/FileTypeIcon";
import type { DocFormat } from "@rightkit/document-core/documents/types";
import {
  explorerListDir,
  explorerUnwatchDir,
  explorerWatchDir,
  onExplorerChanged,
  type ExplorerEntry,
} from "../../../state/explorer";
import { parseHostError } from "@rightkit/document-core/state/tauriBridge";
import { childNameOnPath, pathInside, rowKindOf } from "./explorerState";
import { hostAppName } from "../../../appName";

// ── Data model ─────────────────────────────────────────────────────────────

/** A node the tree can render: a root (location / pinned dir / pinned file) or a listing entry. */
export interface ExplorerNodeSpec {
  path: string;
  name: string;
  kind: "dir" | "file" | "other";
  /** Format id for file rows (FileTypeIcon); undefined for dirs/others. */
  format?: string;
  /** Location kind ("home", "desktop", …) driving the root icon; undefined for plain dirs. */
  icon?: string;
}

export interface ExplorerDirListing {
  status: "loading" | "ready" | "error";
  entries: ExplorerEntry[];
  truncated: boolean;
  message?: string;
}

export interface ExplorerTreeApi {
  /** Dir paths currently expanded (persisted under vr.explorer.v1). */
  expanded: ReadonlySet<string>;
  /** dir path → latest listing state. */
  dirs: ReadonlyMap<string, ExplorerDirListing>;
  expandDir: (path: string) => void;
  collapseDir: (path: string) => void;
  toggleDir: (path: string) => void;
  expandPaths: (paths: Iterable<string>) => void;
  /** Force a re-list of a dir (watcher events; also retries after an error). */
  relistDir: (path: string) => void;
}

function explorerErrorMessage(e: unknown): string {
  // Commands reject with the typed {code,message} envelope (e.g. "permission_denied") — show its
  // message inline in the tree rather than toasting or crashing.
  return parseHostError(e).message;
}

/** `wantedPath` is the active file; a folder on its way is listed with the next segment's name so
 *  the backend keeps that row even past its entry cap (otherwise the reveal stops at that folder). */
export function useExplorerTree(wantedPath: string | null = null): ExplorerTreeApi {
  const [expanded, setExpanded] = useState<ReadonlySet<string>>(() => new Set());
  const [dirs, setDirs] = useState<ReadonlyMap<string, ExplorerDirListing>>(() => new Map());
  const expandedRef = useRef(expanded);
  expandedRef.current = expanded;
  const dirsRef = useRef(dirs);
  dirsRef.current = dirs;
  const wantedRef = useRef(wantedPath);
  wantedRef.current = wantedPath;
  // Synchronous guards so a double-fired effect (StrictMode) or rapid toggle can't double-list or
  // double-watch a dir before the state update lands.
  const watchedRef = useRef<Set<string>>(new Set());
  const loadingRef = useRef<Set<string>>(new Set());

  const setDir = useCallback((path: string, next: ExplorerDirListing) => {
    setDirs((prev) => {
      const m = new Map(prev);
      m.set(path, next);
      return m;
    });
  }, []);

  const fetchDir = useCallback(
    (path: string) => {
      if (loadingRef.current.has(path)) return;
      loadingRef.current.add(path);
      const cur = dirsRef.current.get(path);
      setDir(path, {
        status: "loading",
        entries: cur?.entries ?? [],
        truncated: cur?.truncated ?? false,
      });
      void explorerListDir(path, childNameOnPath(path, wantedRef.current)).then(
        (listing) => {
          loadingRef.current.delete(path);
          setDir(path, {
            status: "ready",
            // A folder never lists itself: a child carrying its parent's path (a link that
            // resolves back to the folder) would make an expanded folder render inside itself
            // without end.
            entries: keepUnchangedEntries(
              dirsRef.current.get(path)?.entries,
              listing.entries.filter((e) => e.path !== path),
            ),
            truncated: listing.truncated,
          });
        },
        (e: unknown) => {
          loadingRef.current.delete(path);
          setDir(path, {
            status: "error",
            entries: cur?.entries ?? [],
            truncated: false,
            message: explorerErrorMessage(e),
          });
        },
      );
    },
    [setDir],
  );

  /** List a dir on first expand; a later manual relistDir forces a fetch. */
  const loadDir = useCallback(
    (path: string) => {
      const cur = dirsRef.current.get(path);
      if (cur && cur.status !== "error") return;
      fetchDir(path);
    },
    [fetchDir],
  );

  const watch = useCallback((path: string) => {
    if (watchedRef.current.has(path)) return;
    watchedRef.current.add(path);
    void explorerWatchDir(path);
  }, []);

  const unwatch = useCallback((path: string) => {
    if (!watchedRef.current.delete(path)) return;
    void explorerUnwatchDir(path);
  }, []);

  const expandDir = useCallback(
    (path: string) => {
      setExpanded((prev) => {
        if (prev.has(path)) return prev;
        const next = new Set(prev);
        next.add(path);
        return next;
      });
      watch(path);
      loadDir(path);
    },
    [watch, loadDir],
  );

  const collapseDir = useCallback(
    (path: string) => {
      setExpanded((prev) => {
        if (!prev.has(path)) return prev;
        const next = new Set(prev);
        next.delete(path);
        return next;
      });
      unwatch(path);
    },
    [unwatch],
  );

  const toggleDir = useCallback(
    (path: string) => {
      if (expandedRef.current.has(path)) collapseDir(path);
      else expandDir(path);
    },
    [expandDir, collapseDir],
  );

  const expandPaths = useCallback(
    (paths: Iterable<string>) => {
      for (const p of paths) expandDir(p);
    },
    [expandDir],
  );

  // One global "explorer:changed" subscription: re-list only the changed dir, and only while it's
  // expanded (its listing is on screen). On unmount, drop the listener and every dir watch.
  useEffect(() => {
    let live = true;
    let un: (() => void) | undefined;
    void onExplorerChanged((dir) => {
      if (!live || !expandedRef.current.has(dir)) return;
      fetchDir(dir);
    }).then((unsub) => {
      if (live) un = unsub;
      else unsub();
    });
    return () => {
      live = false;
      un?.();
      for (const w of watchedRef.current) void explorerUnwatchDir(w);
      watchedRef.current.clear();
    };
  }, [fetchDir]);

  return useMemo(
    () => ({
      expanded,
      dirs,
      expandDir,
      collapseDir,
      toggleDir,
      expandPaths,
      relistDir: fetchDir,
    }),
    [expanded, dirs, expandDir, collapseDir, toggleDir, expandPaths, fetchDir],
  );
}

/** A re-list returns fresh objects for every entry. Keep the previous object for each entry that
 *  did not change, so memoised rows skip it: a watched folder that changes constantly (a temp or
 *  build directory) otherwise re-rendered every one of its rows on every change. */
function keepUnchangedEntries(
  previous: ExplorerEntry[] | undefined,
  next: ExplorerEntry[],
): ExplorerEntry[] {
  if (!previous || previous.length === 0) return next;
  const byPath = new Map(previous.map((e) => [e.path, e]));
  return next.map((e) => {
    const old = byPath.get(e.path);
    return old &&
      old.name === e.name &&
      old.kind === e.kind &&
      old.modified === e.modified &&
      old.size === e.size
      ? old
      : e;
  });
}

// ── View ───────────────────────────────────────────────────────────────────

/** Row callbacks with a stable identity for the life of the tree. Rows are memoised on their own
 *  props, so nothing a row receives may change when unrelated tree state changes: a row that
 *  re-rendered on every listing, focus move or tab switch made a large folder freeze the app. */
interface RowActions {
  toggleDir: (path: string) => void;
  setFocusPath: (path: string) => void;
  onActivateRow: (node: ExplorerNodeSpec) => void;
  onOpenMenu: (node: ExplorerNodeSpec, x: number, y: number) => void;
}

interface TreeCtx {
  tree: ExplorerTreeApi;
  focusPath: string | null;
  firstPath: string | undefined;
  activePath: string | null;
  /** File the sidebar is scrolling to; its row is mounted with the first batch. */
  revealPath: string | null;
  openPaths: ReadonlySet<string>;
  actions: RowActions;
}

/** Rows of a folder mount in batches of this size, one batch per frame, so expanding a folder
 *  with thousands of entries never blocks the page in a single render. */
const ROW_MOUNT_BATCH = 200;

function locationIcon(icon: string | undefined): ReactNode {
  const props = { size: 15, weight: "regular" as const, "aria-hidden": true as const };
  switch (icon) {
    case "home":
      return <House {...props} />;
    case "desktop":
      return <Desktop {...props} />;
    case "documents":
      return <FileText {...props} />;
    case "downloads":
      return <DownloadSimple {...props} />;
    case "icloud":
    case "onedrive":
      return <Cloud {...props} />;
    case "volume":
    case "root":
      return <HardDrives {...props} />;
    default:
      return <Folder {...props} />;
  }
}

function nodeIcon(node: ExplorerNodeSpec): ReactNode {
  if (node.kind === "dir") {
    if (node.icon !== undefined) return locationIcon(node.icon);
    return <Folder size={15} weight="fill" aria-hidden />;
  }
  if (node.kind === "file") {
    return (
      <FileTypeIcon format={(node.format ?? "unknown") as DocFormat} path={node.path} size={14} />
    );
  }
  return <File size={15} weight="regular" aria-hidden />;
}

const TreeRow = memo(function TreeRow({
  node,
  depth,
  parentPath,
  expanded,
  selected,
  open,
  tabbed,
  actions,
}: {
  node: ExplorerNodeSpec;
  depth: number;
  parentPath: string | undefined;
  expanded: boolean;
  selected: boolean;
  open: boolean;
  tabbed: boolean;
  actions: RowActions;
}) {
  const expandable = node.kind === "dir";
  const unsupported = node.kind === "other";
  return (
    <div
      role="treeitem"
      aria-label={node.name}
      aria-description={open ? "Open in a tab" : undefined}
      aria-expanded={expandable ? expanded : undefined}
      aria-selected={selected}
      aria-disabled={unsupported || undefined}
      data-vr-tree-path={node.path}
      data-vr-parent={parentPath}
      tabIndex={tabbed ? 0 : -1}
      title={unsupported ? `${node.path} — ${hostAppName()} can't open this file type` : node.path}
      className={`vr-x-row${unsupported ? " vr-x-dim" : ""}`}
      style={{ paddingLeft: 8 + depth * 14 }}
      onClick={() => {
        if (expandable) actions.toggleDir(node.path);
        else if (node.kind === "file") actions.onActivateRow(node);
      }}
      onFocus={() => actions.setFocusPath(node.path)}
      onContextMenu={(e) => {
        e.preventDefault();
        actions.onOpenMenu(node, e.clientX, e.clientY);
      }}
    >
      <span className="vr-x-caret" aria-hidden>
        {expandable ? (
          expanded ? (
            <CaretDown size={10} weight="regular" />
          ) : (
            <CaretRight size={10} weight="regular" />
          )
        ) : null}
      </span>
      <span className="vr-x-icn" aria-hidden>
        {nodeIcon(node)}
      </span>
      <span className="vr-x-name">{node.name}</span>
      {open && <span className="vr-x-open" title="Open in a tab" aria-hidden />}
      <button
        type="button"
        tabIndex={-1}
        aria-label={`Actions for ${node.name}`}
        className="vr-x-dots vr-press vr-focus"
        onClick={(e: ReactMouseEvent<HTMLButtonElement>) => {
          e.stopPropagation();
          const r = e.currentTarget.getBoundingClientRect();
          actions.onOpenMenu(node, r.right, r.bottom + 2);
        }}
        onContextMenu={(e) => {
          e.preventDefault();
          e.stopPropagation();
          const r = e.currentTarget.getBoundingClientRect();
          actions.onOpenMenu(node, r.right, r.bottom + 2);
        }}
      >
        <DotsThree size={14} weight="bold" aria-hidden />
      </button>
    </div>
  );
});

// One spec object per listing entry, so a memoised row sees the same `node` on every render.
const specByEntry = new WeakMap<ExplorerEntry, ExplorerNodeSpec>();

function entryToSpec(e: ExplorerEntry): ExplorerNodeSpec {
  const cached = specByEntry.get(e);
  if (cached) return cached;
  const kind = rowKindOf(e.kind);
  const spec: ExplorerNodeSpec = {
    path: e.path,
    name: e.name,
    kind,
    format: kind === "file" ? e.kind : undefined,
  };
  specByEntry.set(e, spec);
  return spec;
}

function ChildrenRows({ dir, depth, ctx }: { dir: string; depth: number; ctx: TreeCtx }) {
  const st = ctx.tree.dirs.get(dir);
  const pad = { paddingLeft: 24 + depth * 14 };
  const total = st?.entries.length ?? 0;
  const [mounted, setMounted] = useState(ROW_MOUNT_BATCH);
  useEffect(() => {
    if (mounted >= total) return;
    const frame = requestAnimationFrame(() =>
      setMounted((count) => Math.min(total, count + ROW_MOUNT_BATCH)),
    );
    return () => cancelAnimationFrame(frame);
  }, [mounted, total]);
  // The row on the way to the file being revealed (or the active file) must exist at once, so the
  // reveal can scroll to it; rows mounted for it stay mounted once the reveal has finished.
  const wanted = ctx.revealPath ?? ctx.activePath;
  const wantedIndex = useMemo(() => {
    if (!st || !wanted || total <= ROW_MOUNT_BATCH) return -1;
    return st.entries.findIndex((e) => e.path === wanted || pathInside(e.path, wanted));
  }, [st, wanted, total]);
  const limit = Math.min(total, Math.max(mounted, wantedIndex + 1));
  useEffect(() => {
    if (limit > mounted) setMounted(limit);
  }, [limit, mounted]);
  if (!st || (st.status === "loading" && st.entries.length === 0)) {
    return (
      <div className="vr-x-empty" style={pad}>
        Loading…
      </div>
    );
  }
  if (st.status === "error") {
    return (
      <div className="vr-x-note" role="note" style={pad}>
        {st.message ?? "Couldn't list this folder."}
      </div>
    );
  }
  return (
    <>
      {st.entries.length === 0 && (
        <div className="vr-x-empty" style={pad}>
          Empty folder
        </div>
      )}
      {(limit < total ? st.entries.slice(0, limit) : st.entries).map((e) => (
        <ExplorerNode key={e.path} node={entryToSpec(e)} depth={depth} parentPath={dir} ctx={ctx} />
      ))}
      {st.truncated && limit >= total && (
        <div className="vr-x-empty" style={pad}>
          … (more)
        </div>
      )}
    </>
  );
}

function ExplorerNode({
  node,
  depth,
  parentPath,
  ctx,
}: {
  node: ExplorerNodeSpec;
  depth: number;
  parentPath: string | undefined;
  ctx: TreeCtx;
}) {
  const expanded = node.kind === "dir" && ctx.tree.expanded.has(node.path);
  const selected = node.path === ctx.activePath;
  const open = !selected && node.kind !== "dir" && ctx.openPaths.has(node.path);
  const tabbed =
    ctx.focusPath === node.path || (ctx.focusPath == null && ctx.firstPath === node.path);
  return (
    <>
      <TreeRow
        node={node}
        depth={depth}
        parentPath={parentPath}
        expanded={expanded}
        selected={selected}
        open={open}
        tabbed={tabbed}
        actions={ctx.actions}
      />
      {expanded && (
        <div role="group">
          <ChildrenRows dir={node.path} depth={depth + 1} ctx={ctx} />
        </div>
      )}
    </>
  );
}

/** One ARIA tree (roving tabindex, arrow keys) over a set of root nodes. Used for the Locations
 *  section (roots = explorer_locations) and for the Pinned section (roots = pinned dirs+files). */
export function ExplorerTree({
  ariaLabel,
  roots,
  tree,
  activePath,
  revealPath = null,
  openPaths,
  onActivateRow,
  onOpenMenu,
}: {
  ariaLabel: string;
  roots: ExplorerNodeSpec[];
  tree: ExplorerTreeApi;
  activePath: string | null;
  /** File the sidebar is currently scrolling to (see ExplorerSections' active-file reveal). */
  revealPath?: string | null;
  openPaths: ReadonlySet<string>;
  /** Leaf activation — supported file rows only (dirs toggle internally). */
  onActivateRow: (node: ExplorerNodeSpec) => void;
  onOpenMenu: (node: ExplorerNodeSpec, x: number, y: number) => void;
}) {
  const ref = useRef<HTMLDivElement>(null);
  const [focusPath, setFocusPath] = useState<string | null>(null);

  // The latest callbacks behind one object whose identity never changes (see RowActions).
  const latest = useRef({ toggleDir: tree.toggleDir, onActivateRow, onOpenMenu });
  latest.current = { toggleDir: tree.toggleDir, onActivateRow, onOpenMenu };
  const actions = useMemo<RowActions>(
    () => ({
      toggleDir: (path) => latest.current.toggleDir(path),
      setFocusPath,
      onActivateRow: (node) => latest.current.onActivateRow(node),
      onOpenMenu: (node, x, y) => latest.current.onOpenMenu(node, x, y),
    }),
    [],
  );

  const ctx: TreeCtx = useMemo(
    () => ({
      tree,
      focusPath,
      firstPath: roots[0]?.path,
      activePath,
      revealPath,
      openPaths,
      actions,
    }),
    [tree, focusPath, roots, activePath, revealPath, openPaths, actions],
  );

  const onKeyDown = (e: ReactKeyboardEvent<HTMLDivElement>) => {
    const container = ref.current;
    if (!container) return;
    const items = Array.from(container.querySelectorAll<HTMLElement>('[role="treeitem"]'));
    if (items.length === 0) return;
    const activeEl = document.activeElement as HTMLElement | null;
    let idx = items.indexOf(activeEl as HTMLElement);
    if (idx < 0) idx = items.findIndex((el) => el.dataset.vrTreePath === focusPath);
    if (idx < 0) idx = 0;
    const cur = items[idx];
    const focusItem = (el: HTMLElement | undefined) => {
      if (!el) return;
      el.focus();
      el.scrollIntoView?.({ block: "nearest" });
    };
    switch (e.key) {
      case "ArrowDown":
        e.preventDefault();
        focusItem(items[Math.min(items.length - 1, idx + 1)]);
        break;
      case "ArrowUp":
        e.preventDefault();
        focusItem(items[Math.max(0, idx - 1)]);
        break;
      case "Home":
        e.preventDefault();
        focusItem(items[0]);
        break;
      case "End":
        e.preventDefault();
        focusItem(items[items.length - 1]);
        break;
      case "ArrowRight": {
        e.preventDefault();
        const state = cur.getAttribute("aria-expanded");
        const path = cur.dataset.vrTreePath;
        if (state === "false" && path) tree.expandDir(path);
        else if (state === "true") focusItem(items[idx + 1]);
        break;
      }
      case "ArrowLeft": {
        e.preventDefault();
        const state = cur.getAttribute("aria-expanded");
        const path = cur.dataset.vrTreePath;
        if (state === "true" && path) {
          tree.collapseDir(path);
          break;
        }
        const parent = cur.dataset.vrParent;
        if (parent) {
          const parentEl = items.find((el) => el.dataset.vrTreePath === parent);
          if (parentEl) focusItem(parentEl);
        }
        break;
      }
      case "Enter":
      case " ": {
        e.preventDefault();
        cur.click();
        break;
      }
    }
  };

  return (
    <div role="tree" aria-label={ariaLabel} ref={ref} onKeyDown={onKeyDown} className="vr-x-tree">
      {/* Two locations can resolve to the same folder (e.g. Desktop falling back to Home), so
          the root key carries its position, not just the path. */}
      {roots.map((r, i) => (
        <ExplorerNode key={`${i}:${r.path}`} node={r} depth={0} parentPath={undefined} ctx={ctx} />
      ))}
    </div>
  );
}
