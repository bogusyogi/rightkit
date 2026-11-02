// The expanded sidebar shell: titlebar row (drag region + collapse button), the slim New/Open/
// Search action row, the explorer sections (Pinned / Recent / Locations lazy tree), the footer
// (wordmark + settings + theme), and the width drag handle. Open documents live in the titlebar
// tab strip now — this panel only shows files on disk.
import {
  useEffect,
  useRef,
  useState,
  type PointerEvent as ReactPointerEvent,
  type ReactNode,
} from "react";
import { X } from "@phosphor-icons/react/dist/icons/X";
import { Gear } from "@phosphor-icons/react/dist/icons/Gear";
import { MagnifyingGlass } from "@phosphor-icons/react/dist/icons/MagnifyingGlass";
import { Palette } from "@phosphor-icons/react/dist/icons/Palette";
import { FolderOpen } from "@phosphor-icons/react/dist/icons/FolderOpen";
import { IconButton } from "@rightkit/react-shell/react";
import { useDismiss } from "@rightkit/platform-ui/headless";
import { AppearanceMenu } from "../../settings/AppearanceMenu.js";
import type { SidebarProps } from "../Sidebar.js";
import { PanelIcon, PlusIcon } from "./SidebarIcons.js";
import { NewOpenMenu } from "./SidebarMenus.js";
import { ExplorerSections } from "./explorer/ExplorerSections.js";

const HEADER_BTN =
  "vr-press vr-focus flex h-7 min-w-7 items-center justify-center rounded px-1 font-sans text-xs text-muted hover:bg-ink/40 hover:text-paper";

export function SidebarExpandedPanel({
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
  onOpenDialog,
  onOpenUrl,
  onOpenClipboard,
  onNew,
  onOpenSettings,
  titlebarSlot,
  sidebarWidth,
  startWidthResize,
  setOpen,
  SIDEBAR_MIN_WIDTH,
  SIDEBAR_MAX_WIDTH,
}: SidebarProps & {
  sidebarWidth: number;
  startWidthResize: (event: ReactPointerEvent<HTMLDivElement>) => void;
  setOpen: (open: boolean) => void;
  SIDEBAR_MIN_WIDTH: number;
  SIDEBAR_MAX_WIDTH: number;
}): ReactNode {
  const [searchOpen, setSearchOpen] = useState(false);
  const [searchQuery, setSearchQuery] = useState("");
  const searchInputRef = useRef<HTMLInputElement>(null);
  const searchButtonRef = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    if (searchOpen) searchInputRef.current?.focus();
  }, [searchOpen]);
  const closeSearch = () => {
    setSearchOpen(false);
    setSearchQuery("");
    searchButtonRef.current?.focus();
  };
  // Escape in the search field closes it (bubble phase on the input; stops propagation as before).
  useDismiss(closeSearch, {
    enabled: searchOpen,
    escapeTarget: searchInputRef,
    outside: false,
    imeGuard: false,
  });
  const plusButtonRef = useRef<HTMLButtonElement>(null);
  const themeButtonRef = useRef<HTMLButtonElement>(null);
  const [plusOpen, setPlusOpen] = useState(false);
  const [appearanceOpen, setAppearanceOpen] = useState(false);

  return (
    <aside
      data-vr-sidebar="expanded"
      className="no-print vr-chrome vr-sidebar-shell relative flex shrink-0 flex-col bg-panel"
      style={{ width: sidebarWidth }}
    >
      {/* On macOS the native traffic lights sit top-left, so the collapse button goes to the RIGHT
          edge (clearly separated from the lights). The empty left side remains a drag region. */}
      <div
        data-vr-sidebar-titlebar
        data-tauri-drag-region
        className="relative flex h-[52px] items-center justify-end gap-1 px-3"
      >
        {titlebarSlot && <div className="mr-auto flex min-w-0 items-center">{titlebarSlot}</div>}
        <IconButton label="Collapse sidebar" onClick={() => setOpen(false)} className={HEADER_BTN}>
          <PanelIcon />
        </IconButton>
      </div>

      {/* Slim action row (mockup .s-actions): New button (formats + Open file… menu) + search +
          open-file affordances. */}
      <div className="vr-x-actions">
        <button
          type="button"
          ref={plusButtonRef}
          className="vr-x-new vr-press vr-focus"
          aria-haspopup="menu"
          aria-expanded={plusOpen}
          title="New document"
          onClick={() => setPlusOpen((o) => !o)}
        >
          <PlusIcon /> New
        </button>
        <span className="flex-1" />
        <IconButton
          ref={searchButtonRef}
          label="Search documents"
          aria-expanded={searchOpen}
          aria-controls="vr-sidebar-document-search"
          className="vr-press vr-focus flex h-7 min-w-7 items-center justify-center rounded px-1 text-muted hover:bg-ink/40 hover:text-paper"
          onClick={() => (searchOpen ? closeSearch() : setSearchOpen(true))}
        >
          <MagnifyingGlass size={14} weight="regular" />
        </IconButton>
        <IconButton label="Open file…" onClick={onOpenDialog} className={HEADER_BTN}>
          <FolderOpen size={14} weight="regular" />
        </IconButton>
      </div>
      {plusOpen && (
        <NewOpenMenu
          anchor={plusButtonRef.current ?? undefined}
          onClose={() => setPlusOpen(() => false)}
          onNew={onNew}
          onOpenDialog={onOpenDialog}
          onOpenUrl={onOpenUrl}
          onOpenClipboard={onOpenClipboard}
        />
      )}

      {searchOpen && (
        <div
          id="vr-sidebar-document-search"
          role="search"
          aria-label="Search sidebar documents"
          className="mx-3 mb-2 flex shrink-0 items-center gap-1 rounded border border-line bg-panel-2 px-2 focus-within:border-copper"
        >
          <input
            ref={searchInputRef}
            aria-label="Document name"
            placeholder="Search documents…"
            value={searchQuery}
            onChange={(event) => setSearchQuery(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "ArrowDown" || event.key === "Enter") {
                const first = event.currentTarget
                  .closest("aside")
                  ?.querySelector<HTMLButtonElement>("[data-vr-document-search-results] button");
                if (first) {
                  event.preventDefault();
                  if (event.key === "Enter") first.click();
                  else first.focus();
                }
              }
            }}
            className="min-w-0 flex-1 bg-transparent py-1.5 font-sans text-xs text-paper outline-none placeholder:text-muted"
          />
          <IconButton
            label="Close document search"
            onClick={closeSearch}
            className="vr-focus flex h-6 w-6 shrink-0 items-center justify-center rounded text-muted hover:text-paper"
          >
            <X size={12} />
          </IconButton>
        </div>
      )}

      <ExplorerSections
        searchQuery={searchQuery}
        activePath={activePath}
        openPaths={openPaths}
        recents={recents}
        pinnedPaths={pinnedPaths}
        pinnedEntries={pinnedEntries}
        onActivate={onActivate}
        onOpenRecent={onOpenRecent}
        onOpenPath={onOpenPath}
        onTogglePin={onTogglePin}
        onTogglePinDir={onTogglePinDir}
        onClearRecents={onClearRecents}
      />

      {/* Footer — wordmark on the left, settings + appearance as icon buttons on the right. */}
      <div className="relative flex items-center justify-between border-t border-line px-3 py-2">
        <span
          data-vr-sidebar-footer-brand
          className="min-w-0 truncate px-1 [font-family:var(--font-wordmark)] text-xl leading-none tracking-[0.01em] text-paper"
        >
          View<span className="text-copper">Right</span>
        </span>
        <div className="flex items-center gap-1">
          <IconButton
            label="Settings"
            onClick={onOpenSettings}
            data-vr-tooltip="Settings"
            data-vr-tooltip-side="top"
            className="vr-icon-tooltip vr-press vr-focus flex h-8 w-8 items-center justify-center rounded text-muted hover:bg-ink/40 hover:text-paper"
          >
            <Gear size={14} weight="regular" />
          </IconButton>
          <IconButton
            ref={themeButtonRef}
            label="Theme"
            onClick={() => setAppearanceOpen((o) => !o)}
            aria-haspopup="dialog"
            aria-expanded={appearanceOpen}
            data-vr-tooltip="Theme"
            data-vr-tooltip-side="top"
            className="vr-icon-tooltip vr-press vr-focus flex h-8 w-8 items-center justify-center rounded text-muted hover:bg-ink/40 hover:text-paper"
          >
            <Palette size={14} weight="regular" />
          </IconButton>
        </div>
        {appearanceOpen && (
          <AppearanceMenu
            anchor={themeButtonRef.current ?? undefined}
            onClose={() => setAppearanceOpen(() => false)}
            onMore={onOpenSettings}
          />
        )}
      </div>
      <div
        role="separator"
        tabIndex={0}
        aria-label="Resize sidebar"
        aria-orientation="vertical"
        aria-valuemin={SIDEBAR_MIN_WIDTH}
        aria-valuemax={SIDEBAR_MAX_WIDTH}
        aria-valuenow={Math.round(sidebarWidth)}
        aria-valuetext={`${Math.round(sidebarWidth)} pixels wide`}
        onPointerDown={startWidthResize}
        onKeyDown={(event) => {
          // Arrow keys resize through the SAME drag state machine the pointer path uses:
          // a plain-object pointerdown (its currentTarget lacks setPointerCapture, so the
          // capture call is skipped), then real pointermove/pointerup events so the window
          // listeners apply, clamp, and persist the delta exactly as a mouse drag would.
          if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") return;
          if (typeof PointerEvent === "undefined") return;
          event.preventDefault();
          const step = event.key === "ArrowRight" ? 16 : -16;
          const x = event.currentTarget.getBoundingClientRect().left;
          startWidthResize({
            preventDefault() {},
            currentTarget: {},
            pointerId: -1,
            clientX: x,
          } as unknown as ReactPointerEvent<HTMLDivElement>);
          window.dispatchEvent(
            new PointerEvent("pointermove", { pointerId: -1, clientX: x + step }),
          );
          window.dispatchEvent(new PointerEvent("pointerup", { pointerId: -1 }));
        }}
        className="absolute bottom-0 right-0 top-0 z-30 w-2 translate-x-1 cursor-col-resize"
        title="Resize sidebar"
      />
    </aside>
  );
}
