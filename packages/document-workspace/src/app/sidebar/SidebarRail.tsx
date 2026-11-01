// The collapsed sidebar: a thin always-visible rail fused with the titlebar, so the everyday
// actions never move into the tab strip. Top: expand, New (format/open menu), Recent. Middle:
// pinned files (one tap opens). Bottom: Settings. Open documents stay in the titlebar tabs.
import { useRef, useState, type ReactNode } from "react";
import { IconButton } from "@rightkit/react-shell/react";
import { Gear } from "@phosphor-icons/react/dist/icons/Gear";
import { FolderSimple } from "@phosphor-icons/react/dist/icons/FolderSimple";
import { FileTypeIcon } from "../../documents/FileTypeIcon";
import type { DocFormat } from "@rightkit/document-core/documents/types";
import { shortcutLabel } from "@rightkit/document-core/platform";
import type { SidebarProps } from "../Sidebar";
import { ClockIcon, PanelIcon, PlusIcon } from "./SidebarIcons";
import { NewOpenMenu, RecentsMenu } from "./SidebarMenus";

const RAIL_WIDTH = 52;

export function SidebarRail({
  activePath,
  recents,
  pinnedEntries,
  onActivate,
  onOpenRecent,
  onOpenPath,
  onClearRecents,
  onOpenDialog,
  onOpenUrl,
  onOpenClipboard,
  onNew,
  onOpenSettings,
  openPaths,
  setOpen,
}: SidebarProps & { setOpen: (open: boolean) => void }): ReactNode {
  const [menu, setMenu] = useState<"new" | "recents" | null>(null);
  const anchorRef = useRef<HTMLElement | null>(null);
  const toggle = (which: "new" | "recents", button: HTMLElement) => {
    anchorRef.current = button;
    setMenu((m) => (m === which ? null : which));
  };

  return (
    <aside
      data-vr-sidebar="rail"
      aria-label="Sidebar"
      className="no-print vr-chrome vr-sidebar-shell relative flex shrink-0 flex-col items-center bg-panel"
      style={{ width: RAIL_WIDTH }}
    >
      {/* Titlebar band: the native traffic lights sit here on macOS; it stays a drag region. */}
      <div data-tauri-drag-region className="h-[52px] w-full shrink-0" />
      <div className="flex flex-col items-center gap-1">
        <RailButton
          label={`Show sidebar (${shortcutLabel("mod+\\")})`}
          onClick={() => setOpen(true)}
        >
          <PanelIcon />
        </RailButton>
        <RailButton label="New document" expanded={menu === "new"} onClick={(button) => toggle("new", button)}>
          <PlusIcon />
        </RailButton>
        <RailButton
          label="Recent files"
          expanded={menu === "recents"}
          onClick={(button) => toggle("recents", button)}
        >
          <ClockIcon />
        </RailButton>
      </div>

      {pinnedEntries.length > 0 && (
        <>
          <span aria-hidden="true" className="my-2 h-px w-6 shrink-0 bg-line" />
          <nav
            aria-label="Pinned"
            className="flex min-h-0 flex-1 flex-col items-center gap-1 overflow-y-auto overflow-x-hidden [direction:rtl] [&>*]:[direction:ltr]"
          >
            {pinnedEntries.map((pin) => {
              const isDir = pin.kind === "dir";
              const open = openPaths.has(pin.path);
              return (
                <RailButton
                  key={pin.path}
                  label={pin.displayName}
                  current={pin.path === activePath}
                  onClick={() => {
                    // A pinned folder needs the explorer to browse it; a file opens directly.
                    if (isDir) setOpen(true);
                    else if (open) onActivate(pin.path);
                    else onOpenPath(pin.path);
                  }}
                >
                  {isDir ? (
                    <FolderSimple size={16} weight="regular" aria-hidden />
                  ) : (
                    <FileTypeIcon format={pin.format as DocFormat} path={pin.path} size={16} />
                  )}
                </RailButton>
              );
            })}
          </nav>
        </>
      )}
      <span className="flex-1" />
      <div className="mb-2 flex shrink-0 flex-col items-center">
        <RailButton label="Settings" onClick={onOpenSettings}>
          <Gear size={15} weight="regular" />
        </RailButton>
      </div>

      {menu === "new" && (
        <NewOpenMenu
          anchor={anchorRef.current ?? undefined}
          onClose={() => setMenu(null)}
          onNew={onNew}
          onOpenDialog={onOpenDialog}
          onOpenUrl={onOpenUrl}
          onOpenClipboard={onOpenClipboard}
        />
      )}
      {menu === "recents" && (
        <RecentsMenu
          anchor={anchorRef.current ?? undefined}
          onClose={() => setMenu(null)}
          recents={recents}
          onOpenRecent={(entry) => void onOpenRecent(entry)}
          onClearRecents={onClearRecents}
        />
      )}
    </aside>
  );
}

function RailButton({
  label,
  onClick,
  expanded,
  current,
  children,
}: {
  label: string;
  onClick: (button: HTMLButtonElement) => void;
  expanded?: boolean;
  current?: boolean;
  children: ReactNode;
}) {
  return (
    <IconButton
      label={label}
      onClick={(event) => onClick(event.currentTarget)}
      aria-expanded={expanded}
      aria-current={current ? "page" : undefined}
      data-vr-tooltip={label}
      data-vr-tooltip-side="right"
      className={`vr-icon-tooltip vr-press vr-focus flex h-9 w-9 shrink-0 items-center justify-center rounded-lg text-muted hover:bg-ink/40 hover:text-paper${
        expanded || current ? " bg-ink/40 text-paper" : ""
      }`}
    >
      {children}
    </IconButton>
  );
}
