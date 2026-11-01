// New/Open and Recents popovers. Render identically in the collapsed rail and the expanded header —
// only their anchor button differs. Shared here so the two render sites stay in sync. Both are the
// shell's ContextMenu opened from an anchor (keyboard, dismissal, focus return, viewport fit); the host app supplies the
// glass panel classes and the row look.
import { useMemo, useRef } from "react";
import { ContextMenu, type AnchorRect, type MenuItem } from "@rightkit/react-shell/react";
import type { FileEntry } from "../../state/library.js";
import { FileTypeIcon } from "../../documents/FileTypeIcon.js";
import { menuItemClass, menuPanelClass } from "./SidebarPrimitives.js";

type Anchor = Element | AnchorRect;

const MENU_SLOTS = {
  item: `${menuItemClass} [&.is-active]:bg-ink/50 [&.is-active]:text-paper [&.is-disabled]:cursor-default [&.is-disabled]:hover:bg-transparent`,
  separator: "my-1 h-px bg-line",
};

/**
 * The button a menu hangs off. Call sites pass `anchor` (the clicked button); without it the menu
 * finds its trigger by the label the rail / expanded header give it, which is already in the DOM
 * when the menu mounts.
 */
const TRIGGERS = {
  new: '[data-vr-sidebar] button[aria-label="New document"], [data-vr-sidebar="expanded"] .vr-x-new',
  recents: '[data-vr-sidebar] button[aria-label="Recent files"]',
};
function triggerAnchor(kind: keyof typeof TRIGGERS): Anchor {
  return (
    document.querySelector(TRIGGERS[kind]) ?? { left: 0, top: 52, right: 0, bottom: 52 }
  );
}

export function NewOpenMenu({
  anchor,
  onClose,
  onNew,
  onOpenDialog,
  onOpenUrl,
  onOpenClipboard,
}: {
  anchor?: Anchor;
  onClose: () => void;
  onNew: (format?: "txt" | "md" | "docx") => void;
  onOpenDialog: () => void;
  onOpenUrl: () => void;
  onOpenClipboard: () => void;
}) {
  // Props change identity per render; items must not (exitMs treats a new items identity as a
  // re-open), so run() reads the latest through a ref.
  const cb = useRef({ onNew, onOpenDialog, onOpenUrl, onOpenClipboard });
  cb.current = { onNew, onOpenDialog, onOpenUrl, onOpenClipboard };
  const items: MenuItem[] = useMemo(
    () => [
      { id: "new-txt", label: "New text", run: () => cb.current.onNew("txt") },
      { id: "new-md", label: "New Markdown", run: () => cb.current.onNew("md") },
      { id: "new-docx", label: "New DOCX", run: () => cb.current.onNew("docx") },
      { id: "open", label: "Open file…", run: () => cb.current.onOpenDialog() },
      { id: "open-url", label: "Open URL…", run: () => cb.current.onOpenUrl() },
      { id: "clipboard", label: "From clipboard", run: () => cb.current.onOpenClipboard() },
    ],
    [],
  );
  return (
    <ContextMenu
      anchor={anchor ?? triggerAnchor("new")}
      placement="bottom-start"
      label="New document"
      width={176}
      items={items}
      onClose={onClose}
      itemAs="button"
      showLead={false}
      renderItem={(item) => item.label}
      className={`${menuPanelClass} py-1`}
      scrimClassName="no-print"
      exitMs={80}
      exitClassName="vr-pop-exit"
      slots={MENU_SLOTS}
    />
  );
}

export function RecentsMenu({
  anchor,
  onClose,
  recents,
  onOpenRecent,
  onClearRecents,
}: {
  anchor?: Anchor;
  onClose: () => void;
  recents: FileEntry[];
  onOpenRecent: (entry: FileEntry) => void;
  onClearRecents: () => void;
}) {
  const cb = useRef({ onOpenRecent, onClearRecents });
  cb.current = { onOpenRecent, onClearRecents };
  const shown = useMemo(() => recents.slice(0, 12), [recents]);
  const items: MenuItem[] = useMemo(
    () => [
      { id: "head", label: "Recent", disabled: true },
      ...(shown.length === 0 ? [{ id: "empty", label: "Nothing yet.", disabled: true }] : []),
      ...shown.map((r) => ({ id: `recent:${r.token}`, label: r.displayName, run: () => cb.current.onOpenRecent(r) })),
      ...(shown.length > 0
        ? [{ id: "clear", label: "Clear recent files", separatorBefore: true, run: () => cb.current.onClearRecents() }]
        : []),
    ],
    [shown],
  );
  const byId = useMemo(() => new Map(shown.map((r) => [`recent:${r.token}`, r])), [shown]);
  return (
    <ContextMenu
      anchor={anchor ?? triggerAnchor("recents")}
      placement="bottom-start"
      label="Recent files"
      width={256}
      items={items}
      onClose={onClose}
      itemAs="button"
      showLead={false}
      renderItem={(item) => {
        const entry = byId.get(item.id);
        if (item.id === "head")
          return <span className="font-sans text-chrome-xs font-semibold text-muted-2">{item.label}</span>;
        if (item.id === "empty") return <span className="font-sans text-xs text-muted-2">{item.label}</span>;
        if (!entry) return <span className="text-muted">{item.label}</span>;
        return (
          <>
            <span className="mr-2 shrink-0">
              <FileTypeIcon format={entry.format} path={entry.token} size={21} />
            </span>
            <span className="truncate" title={entry.displayName}>
              {entry.displayName}
            </span>
          </>
        );
      }}
      className={`${menuPanelClass} py-1`}
      scrimClassName="no-print"
      exitMs={80}
      exitClassName="vr-pop-exit"
      slots={MENU_SLOTS}
    />
  );
}
