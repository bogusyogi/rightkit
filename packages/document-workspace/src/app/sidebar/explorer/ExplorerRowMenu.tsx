// Right-click / "…" context menu for explorer tree rows (locations, listing entries, pinned rows)
// and recent rows: Open, Reveal in Finder/File Explorer, Pin/Unpin, Copy Path.
// Same chrome as the document tab menu (SidebarDocumentMenu): the shell's ContextMenu with the
// glass panel and compact rows.
import { useMemo, useRef } from "react";
import { ContextMenu, type MenuItem } from "@rightkit/react-shell/react";
import { explorerReveal } from "../../../state/explorer.js";
import { isMac } from "@rightkit/document-core/platform";
import { compactMenuItemClass, menuPanelClass } from "../SidebarPrimitives.js";
import { copyText } from "../SidebarPathUtils.js";

export interface ExplorerRowMenuState {
  x: number;
  y: number;
  name: string;
  path: string;
  isDir: boolean;
  /** Row kind ("dir" | format id | "other") — drives which items are offered. */
  kind: string;
  pinned: boolean;
  /** Show Pin/Unpin. Hidden for "other" files: their paths carry no broker grant, so the backend
   *  toggle_pin would refuse them anyway. */
  canPin: boolean;
  /** "Open" handler — expand for dirs, open/activate for files. Absent for "other" rows. */
  onOpen?: () => void;
}

export function ExplorerRowMenu({
  menu,
  onClose,
  onTogglePin,
  onTogglePinDir,
}: {
  menu: ExplorerRowMenuState;
  onClose: () => void;
  onTogglePin: (path: string) => void;
  onTogglePinDir: (path: string, label: string) => void;
}) {
  // Callbacks (and menu.onOpen) change identity per render; items must not (exitMs treats a new
  // items identity as a re-open), so run() reads the latest through a ref.
  const cb = useRef({ onOpen: menu.onOpen, onTogglePin, onTogglePinDir });
  cb.current = { onOpen: menu.onOpen, onTogglePin, onTogglePinDir };
  const { path, name, isDir, pinned, canPin } = menu;
  const hasOpen = !!menu.onOpen;
  const items: MenuItem[] = useMemo(
    () => [
      ...(hasOpen ? [{ id: "open", label: "Open", run: () => cb.current.onOpen?.() }] : []),
      { id: "reveal", label: isMac() ? "Reveal in Finder" : "Reveal in File Explorer", run: () => explorerReveal(path) },
      ...(canPin
        ? [
            {
              id: "pin",
              label: pinned ? "Unpin" : "Pin",
              run: () => (isDir ? cb.current.onTogglePinDir(path, name) : cb.current.onTogglePin(path)),
            },
          ]
        : []),
      { id: "copy-path", label: "Copy Path", separatorBefore: true, run: () => copyText(path) },
    ],
    [hasOpen, path, name, isDir, pinned, canPin],
  );

  return (
    <ContextMenu
      x={menu.x}
      y={menu.y}
      items={items}
      onClose={onClose}
      label={`${menu.name} options`}
      width={224}
      itemAs="button"
      showLead={false}
      renderItem={(item) => item.label}
      className={`${menuPanelClass} overflow-y-auto py-0.5`}
      scrimClassName="no-print"
      exitMs={80}
      exitClassName="vr-pop-exit"
      slots={{
        item: `${compactMenuItemClass} [&.is-active]:bg-ink/50 [&.is-active]:text-paper`,
        separator: "my-1 h-px bg-line",
      }}
    />
  );
}
