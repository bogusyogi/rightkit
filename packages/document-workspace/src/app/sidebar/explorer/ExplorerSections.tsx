// The explorer sidebar body: three collapsible sections — Pinned (dirs first, then files;
// pinned dirs expand like tree nodes), Recent (first 5 + "Show more…"), and Locations (the lazy
// directory tree) — inside the sidebar's outer-left-scrollbar nav. Owns section open/closed state,
// the persisted explorer record, active-file reveal, and the row context menu.
import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
  type ReactNode,
  type UIEvent,
} from "react";
import { CaretDown } from "@phosphor-icons/react/dist/icons/CaretDown";
import { FileTypeIcon } from "../../../documents/FileTypeIcon.js";
import { baseName, formatFromPath, type DocFormat } from "@rightkit/document-core/documents/types";
import { explorerLocations, type ExplorerLocation } from "../../../state/explorer.js";
import type { FileEntry, PinEntry } from "../../../state/library.js";
import {
  ancestorDirs,
  explorerPersistedSync,
  loadExplorerPersisted,
  pathInside,
  saveExplorerPersisted,
  type ExplorerSectionState,
} from "./explorerState.js";
import { ExplorerTree, useExplorerTree, type ExplorerNodeSpec } from "./ExplorerTree.js";
import { ExplorerRowMenu, type ExplorerRowMenuState } from "./ExplorerRowMenu.js";

const RECENT_LIMIT = 5;

export interface ExplorerSectionsProps {
  searchQuery?: string;
  activePath: string | null;
  openPaths: ReadonlySet<string>;
  recents: FileEntry[];
  pinnedPaths: ReadonlySet<string>;
  pinnedEntries: PinEntry[];
  onActivate: (path: string) => void;
  onOpenRecent: (entry: FileEntry) => Promise<unknown> | undefined;
  onOpenPath: (path: string) => void;
  onTogglePin: (path: string) => void;
  onTogglePinDir: (path: string, label: string) => void;
  onClearRecents: () => void;
}

function Section({
  id,
  label,
  count,
  open,
  onToggle,
  children,
}: {
  id: keyof ExplorerSectionState;
  label: string;
  count?: number;
  open: boolean;
  onToggle: (id: keyof ExplorerSectionState) => void;
  children: ReactNode;
}) {
  const bodyId = `vr-x-sec-${id}`;
  return (
    <div className={`vr-x-sec${open ? "" : " closed"}`}>
      <button
        type="button"
        className="vr-x-sec-h vr-press"
        aria-expanded={open}
        aria-controls={bodyId}
        onClick={() => onToggle(id)}
      >
        <CaretDown size={10} weight="regular" className="vr-x-sec-caret" aria-hidden />
        <span className="vr-x-sec-label">{label}</span>
        {count != null && count > 0 && <span className="vr-x-sec-count">{count}</span>}
      </button>
      <div id={bodyId} className="vr-x-sec-b">
        {children}
      </div>
    </div>
  );
}

export function ExplorerSections({
  searchQuery = "",
  activePath,
  openPaths,
  recents,
  pinnedPaths,
  pinnedEntries,
  onActivate,
  onOpenRecent,
  onOpenPath,
  onTogglePin,
  onTogglePinDir,
  onClearRecents,
}: ExplorerSectionsProps) {
  const tree = useExplorerTree(activePath);
  const [locations, setLocations] = useState<ExplorerLocation[]>([]);
  const [sections, setSections] = useState<ExplorerSectionState>({
    pinned: true,
    recent: true,
    locations: true,
  });
  const [hydrated, setHydrated] = useState(false);
  const [showAllRecent, setShowAllRecent] = useState(false);
  const [menu, setMenu] = useState<ExplorerRowMenuState | null>(null);
  const [revealPath, setRevealPath] = useState<string | null>(null);
  const scrollRef = useRef<HTMLElement | null>(null);
  const scrollRestored = useRef(false);
  const hydratedRef = useRef(false);
  const revealAttempts = useRef(0);

  // Initial load: location roots + the persisted explorer record (sections + expanded dirs).
  useEffect(() => {
    let live = true;
    void Promise.all([explorerLocations(), loadExplorerPersisted()]).then(([locs, persisted]) => {
      if (!live) return;
      setLocations(locs);
      setSections(persisted.sections);
      tree.expandPaths(persisted.expanded);
      hydratedRef.current = true;
      setHydrated(true);
    });
    return () => {
      live = false;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [tree.expandPaths]);

  // Persist section + expansion changes (debounced inside saveExplorerPersisted); never write
  // before hydration or the defaults would stomp the stored record.
  useEffect(() => {
    if (!hydratedRef.current) return;
    saveExplorerPersisted({ sections });
  }, [sections]);
  useEffect(() => {
    if (!hydratedRef.current) return;
    saveExplorerPersisted({ expanded: [...tree.expanded] });
  }, [tree.expanded]);

  // Restore scroll position once, after hydration.
  useEffect(() => {
    if (!hydrated || scrollRestored.current) return;
    scrollRestored.current = true;
    const el = scrollRef.current;
    const persisted = explorerPersistedSync();
    if (el && persisted.scroll > 0) el.scrollTop = persisted.scroll;
  }, [hydrated]);

  const onScroll = useCallback(
    (e: UIEvent<HTMLElement>) => {
      if (searchQuery.trim()) return;
      saveExplorerPersisted({ scroll: Math.round(e.currentTarget.scrollTop) });
    },
    [searchQuery],
  );

  const toggleSection = useCallback((id: keyof ExplorerSectionState) => {
    setSections((prev) => ({ ...prev, [id]: !prev[id] }));
  }, []);

  // Supported files: focus the tab when already open, else open through the broker-granted path.
  const activateFile = useCallback(
    (path: string) => {
      if (openPaths.has(path)) onActivate(path);
      else onOpenPath(path);
    },
    [openPaths, onActivate, onOpenPath],
  );
  const activateNode = useCallback(
    (node: ExplorerNodeSpec) => activateFile(node.path),
    [activateFile],
  );

  const openMenu = useCallback(
    (node: ExplorerNodeSpec, x: number, y: number) => {
      const isDir = node.kind === "dir";
      setMenu({
        x,
        y,
        name: node.name,
        path: node.path,
        isDir,
        kind: node.kind === "file" ? (node.format ?? "file") : node.kind,
        pinned: pinnedPaths.has(node.path),
        canPin: node.kind !== "other",
        onOpen: isDir
          ? () => tree.expandDir(node.path)
          : node.kind === "file"
            ? () => activateFile(node.path)
            : undefined,
      });
    },
    [pinnedPaths, tree, activateFile],
  );

  const openRecentMenu = useCallback(
    (entry: FileEntry, x: number, y: number) => {
      setMenu({
        x,
        y,
        name: entry.displayName,
        path: entry.token,
        isDir: false,
        kind: entry.format,
        pinned: pinnedPaths.has(entry.token),
        canPin: true,
        onOpen: () => void onOpenRecent(entry),
      });
    },
    [pinnedPaths, onOpenRecent],
  );

  // Root specs.
  const locationRoots = useMemo<ExplorerNodeSpec[]>(
    () =>
      locations.map((l) => ({
        path: l.dir,
        name: l.label,
        kind: "dir" as const,
        icon: l.kind,
      })),
    [locations],
  );
  const pinnedRoots = useMemo<ExplorerNodeSpec[]>(
    () =>
      pinnedEntries.map((p) =>
        p.kind === "dir"
          ? { path: p.path, name: p.displayName, kind: "dir" as const }
          : { path: p.path, name: p.displayName, kind: "file" as const, format: p.format },
      ),
    [pinnedEntries],
  );

  const expandPaths = tree.expandPaths;
  // Active-file reveal: expand the ancestor chain under whichever location contains the file,
  // then scroll the row into view once it exists. Expansion is keyed on dir paths, so the chain
  // can be expanded before any level has finished listing.
  useEffect(() => {
    if (!activePath || locations.length === 0) {
      setRevealPath(null);
      return;
    }
    const root = locations.find((l) => pathInside(l.dir, activePath));
    if (!root) {
      setRevealPath(null);
      return;
    }
    expandPaths(ancestorDirs(activePath, root.dir));
    revealAttempts.current = 0;
    setRevealPath(activePath);
    // Keyed on the active file only: depending on the whole tree object re-ran this on every
    // expand/collapse and re-opened the active file's folders the moment the user closed them.
  }, [activePath, locations, expandPaths]);

  // Whenever the tree changes (children listed, dirs expanded), try to land the reveal scroll.
  // biome-ignore lint/correctness/useExhaustiveDependencies: tree.dirs/tree.expanded are deliberate triggers — the reveal retries scrollIntoView as lazy listings land
  useEffect(() => {
    if (!revealPath) return;
    const el = scrollRef.current;
    if (!el) return;
    const rows = el.querySelectorAll<HTMLElement>("[data-vr-tree-path]");
    for (const row of rows) {
      if (row.dataset.vrTreePath === revealPath) {
        row.scrollIntoView?.({ block: "nearest" });
        setRevealPath(null);
        return;
      }
    }
    // The row may appear only after several lazy listings; don't retry forever.
    revealAttempts.current += 1;
    if (revealAttempts.current > 12) setRevealPath(null);
  }, [revealPath, tree.dirs, tree.expanded]);

  // A pinned file never appears in Recent again — one row per file, Pinned wins.
  const recentFiles = useMemo(
    () => recents.filter((r) => !pinnedPaths.has(r.token)),
    [recents, pinnedPaths],
  );
  const visibleRecents = showAllRecent ? recentFiles : recentFiles.slice(0, RECENT_LIMIT);

  // Filename search reuses known broker entries; it never reads document contents.
  const searchResults = useMemo(() => {
    const files = new Map<
      string,
      { path: string; name: string; format: DocFormat; open: () => void }
    >();
    for (const pin of pinnedEntries) {
      if (pin.kind !== "file") continue;
      files.set(pin.path, {
        path: pin.path,
        name: pin.displayName,
        format: pin.format as DocFormat,
        open: () => activateFile(pin.path),
      });
    }
    for (const entry of recents) {
      const path = entry.displayPath ?? entry.token;
      if (files.has(path)) continue;
      files.set(path, {
        path,
        name: entry.displayName,
        format: entry.format,
        open: () => (openPaths.has(path) ? onActivate(path) : void onOpenRecent(entry)),
      });
    }
    for (const listing of tree.dirs.values()) {
      for (const entry of listing.entries) {
        if (entry.kind === "dir" || entry.kind === "other" || files.has(entry.path)) continue;
        files.set(entry.path, {
          path: entry.path,
          name: entry.name,
          format: entry.kind as DocFormat,
          open: () => activateFile(entry.path),
        });
      }
    }
    for (const path of openPaths) {
      if (files.has(path) || path.startsWith("untitled:") || path.startsWith("web:")) continue;
      files.set(path, {
        path,
        name: baseName(path),
        format: formatFromPath(path),
        open: () => onActivate(path),
      });
    }
    const query = searchQuery.trim().toLocaleLowerCase();
    return [...files.values()].filter((file) => file.name.toLocaleLowerCase().includes(query));
  }, [
    searchQuery,
    pinnedEntries,
    recents,
    tree.dirs,
    openPaths,
    activateFile,
    onActivate,
    onOpenRecent,
  ]);

  // The active file gets exactly ONE highlighted row: the first section that contains it
  // (Pinned → Recent → Locations). Later occurrences fall back to the plain "open in a
  // tab" dot the rows already render for openPaths.
  const activeInPinned =
    !!activePath &&
    pinnedEntries.some(
      (p) => p.path === activePath || (p.kind === "dir" && pathInside(p.path, activePath)),
    );
  const activeInRecent = !!activePath && recentFiles.some((r) => r.token === activePath);
  const pinnedActivePath = activeInPinned ? activePath : null;
  const recentActivePath = !activeInPinned && activeInRecent ? activePath : null;
  const locationsActivePath = !activeInPinned && !activeInRecent ? activePath : null;

  const onRecentKey = (entry: FileEntry) => (e: ReactKeyboardEvent<HTMLElement>) => {
    if (e.key !== "Enter" && e.key !== " ") return;
    e.preventDefault();
    void onOpenRecent(entry);
  };

  return (
    <nav
      ref={scrollRef}
      aria-label="Files"
      data-vr-sidebar-explorer
      onScroll={onScroll}
      className="vr-explorer min-h-0 flex-1 overflow-x-hidden overflow-y-auto font-sans text-paper [direction:rtl] [&>*]:[direction:ltr]"
    >
      {searchQuery.trim() ? (
        <div data-vr-document-search-results>
          <p className="vr-x-empty" role="status">
            {searchResults.length
              ? `${searchResults.length} ${searchResults.length === 1 ? "document" : "documents"}`
              : "No matching documents"}
          </p>
          {searchResults.map((file) => (
            <button
              type="button"
              key={file.path}
              title={file.path}
              data-vr-document-search-path={file.path}
              aria-current={file.path === activePath || undefined}
              className={`vr-x-row vr-x-lrow w-full text-left${file.path === activePath ? " vr-x-sel" : ""}`}
              onClick={file.open}
            >
              <span className="vr-x-caret" aria-hidden />
              <span className="vr-x-icn" aria-hidden>
                <FileTypeIcon format={file.format} path={file.path} size={14} />
              </span>
              <span className="vr-x-name">{file.name}</span>
            </button>
          ))}
        </div>
      ) : (
        <>
          {pinnedRoots.length > 0 && (
            <Section
              id="pinned"
              label="Pinned"
              count={pinnedEntries.length}
              open={sections.pinned}
              onToggle={toggleSection}
            >
              <ExplorerTree
                ariaLabel="Pinned"
                roots={pinnedRoots}
                tree={tree}
                activePath={pinnedActivePath}
                openPaths={openPaths}
                onActivateRow={activateNode}
                onOpenMenu={openMenu}
              />
            </Section>
          )}

          <Section
            id="recent"
            label="Recent"
            count={recentFiles.length}
            open={sections.recent}
            onToggle={toggleSection}
          >
            {recentFiles.length === 0 ? (
              <div className="vr-x-empty">No recent files</div>
            ) : (
              <>
                {visibleRecents.map((r) => {
                  const open = r.token !== recentActivePath && openPaths.has(r.token);
                  return (
                    <div
                      key={r.token}
                      role="button"
                      tabIndex={0}
                      title={r.token}
                      aria-label={r.displayName}
                      aria-description={open ? `${r.token} (open in a tab)` : r.token}
                      data-vr-recent-path={r.token}
                      aria-current={r.token === recentActivePath || undefined}
                      className={`vr-x-row vr-x-lrow${r.token === recentActivePath ? " vr-x-sel" : ""}`}
                      onClick={() => void onOpenRecent(r)}
                      onKeyDown={onRecentKey(r)}
                      onContextMenu={(e) => {
                        e.preventDefault();
                        openRecentMenu(r, e.clientX, e.clientY);
                      }}
                    >
                      <span className="vr-x-caret" aria-hidden />
                      <span className="vr-x-icn" aria-hidden>
                        <FileTypeIcon format={r.format as DocFormat} path={r.token} size={14} />
                      </span>
                      <span className="vr-x-name">{r.displayName}</span>
                      {open && <span className="vr-x-open" title="Open in a tab" aria-hidden />}
                    </div>
                  );
                })}
                {recentFiles.length > RECENT_LIMIT && (
                  <button
                    type="button"
                    className="vr-x-more vr-press vr-focus"
                    onClick={() => setShowAllRecent((v) => !v)}
                  >
                    {showAllRecent
                      ? "Show less"
                      : `Show ${recentFiles.length - RECENT_LIMIT} more…`}
                  </button>
                )}
                <button
                  type="button"
                  className="vr-x-more vr-press vr-focus"
                  onClick={onClearRecents}
                >
                  Clear recent files
                </button>
              </>
            )}
          </Section>

          <Section
            id="locations"
            label="Locations"
            open={sections.locations}
            onToggle={toggleSection}
          >
            {locationRoots.length === 0 ? (
              <div className="vr-x-empty">No locations</div>
            ) : (
              <ExplorerTree
                ariaLabel="Locations"
                roots={locationRoots}
                tree={tree}
                activePath={locationsActivePath}
                revealPath={revealPath}
                openPaths={openPaths}
                onActivateRow={activateNode}
                onOpenMenu={openMenu}
              />
            )}
          </Section>
        </>
      )}

      {menu && (
        <ExplorerRowMenu
          menu={menu}
          onClose={() => setMenu(null)}
          onTogglePin={onTogglePin}
          onTogglePinDir={onTogglePinDir}
        />
      )}
    </nav>
  );
}
