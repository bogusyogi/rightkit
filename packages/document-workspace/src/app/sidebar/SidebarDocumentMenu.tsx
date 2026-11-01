// Right-click / "..." context menu for a single document tab (Open, Close, Close all, Pin, Copy
// Path variants, Reveal in Finder/File Explorer). Rendered from both the collapsed rail and the
// expanded tab list in Sidebar.tsx. The shell's ContextMenu owns keyboard, dismissal (scrim press,
// context-menu gesture), focus return and viewport fit; the host app supplies the rows and the look.
import { useMemo, useRef } from "react";
import { ContextMenu, type MenuItem } from "@rightkit/react-shell/react";
import type { LoadedDocument } from "@rightkit/document-core/documents/types";
import { revealInFileExplorer } from "@rightkit/document-core/state/tauriBridge";
import { isMac } from "@rightkit/document-core/platform";
import { compactMenuItemClass, menuPanelClass } from "./SidebarPrimitives";
import {
  breadcrumbPath,
  copyText,
  isLocalFileDoc,
  pathBase,
  pathFolder,
  relativePath,
} from "./SidebarPathUtils";

export function DocumentMenu({
  menu,
  openLocalPaths,
  pinned,
  onClose,
  onActivate,
  onCloseDoc,
  onCloseOthers,
  onCloseRight,
  onCloseAll,
  onTogglePin,
  titleFor,
}: {
  menu: { doc: LoadedDocument; x: number; y: number };
  openLocalPaths: string[];
  pinned: boolean;
  onClose: () => void;
  onActivate: (path: string) => void;
  onCloseDoc: (path: string) => void;
  /** Rendered only when provided (tab-strip lane): close every other tab. */
  onCloseOthers?: () => void;
  /** Rendered only when provided (tab-strip lane): close tabs to the right of this one. */
  onCloseRight?: () => void;
  onCloseAll: () => void;
  onTogglePin: (path: string) => void;
  titleFor: (doc: LoadedDocument) => string;
}) {
  const { doc, x, y } = menu;
  const local = isLocalFileDoc(doc);
  const pathText = doc.web?.url ?? doc.path;
  // Callbacks change identity every render; items must not (exitMs treats a new items identity as
  // a re-open), so run() reads the latest props through a ref.
  const cb = useRef({ onActivate, onCloseDoc, onCloseOthers, onCloseRight, onCloseAll, onTogglePin });
  cb.current = { onActivate, onCloseDoc, onCloseOthers, onCloseRight, onCloseAll, onTogglePin };
  const hasOthers = !!onCloseOthers;
  const hasRight = !!onCloseRight;
  const docPath = doc.path;
  const isWeb = doc.format === "web";
  const items: MenuItem[] = useMemo(
    () => [
      { id: "open", label: "Open", run: () => cb.current.onActivate(docPath) },
      { id: "close", label: "Close", run: () => cb.current.onCloseDoc(docPath) },
      ...(hasOthers ? [{ id: "close-others", label: "Close other tabs", run: () => cb.current.onCloseOthers?.() }] : []),
      ...(hasRight ? [{ id: "close-right", label: "Close tabs to the right", run: () => cb.current.onCloseRight?.() }] : []),
      { id: "close-all", label: "Close all", run: () => cb.current.onCloseAll() },
      ...(local ? [{ id: "pin", label: pinned ? "Unpin" : "Pin", run: () => cb.current.onTogglePin(docPath) }] : []),
      {
        id: "copy-path",
        label: isWeb ? "Copy URL" : "Copy Path",
        separatorBefore: true,
        run: () => copyText(pathText),
      },
      ...(local
        ? [
            { id: "copy-relative", label: "Copy Relative Path", run: () => copyText(relativePath(docPath, openLocalPaths)) },
            { id: "copy-name", label: "Copy File Name", run: () => copyText(pathBase(docPath)) },
            { id: "copy-folder", label: "Copy Folder Path", run: () => copyText(pathFolder(docPath)) },
            { id: "copy-breadcrumbs", label: "Copy Breadcrumbs Path", run: () => copyText(breadcrumbPath(docPath)) },
            {
              id: "reveal",
              label: isMac() ? "Reveal in Finder" : "Reveal in File Explorer",
              separatorBefore: true,
              run: () => revealInFileExplorer(docPath),
            },
          ]
        : []),
    ],
    [hasOthers, hasRight, local, pinned, isWeb, pathText, docPath, openLocalPaths],
  );

  return (
    <ContextMenu
      x={x}
      y={y}
      items={items}
      onClose={onClose}
      label={`${titleFor(doc)} tab options`}
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
