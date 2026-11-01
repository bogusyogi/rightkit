import { useMemo } from "react";
import { ContextMenu, type MenuItem } from "@rightkit/react-shell/react";
import { useDrive } from "@rightkit/mail-ui/hooks/useDrive";
import { openExternal, type DriveFile } from "@rightkit/mail-ui/lib/ipc";
import { useAppStore } from "@rightkit/mail-ui/store/index";

type DriveFileMenuProps = {
  f: DriveFile;
  anchor: Element;
  onClose: () => void;
  onPreview: () => void;
  drive: ReturnType<typeof useDrive>;
  onMove: (f: DriveFile) => void;
  onShare: (f: DriveFile) => void;
};

/** The per-file dropdown (preview/open/download/attach/rename/move/share/trash). */
export function DriveFileMenu({ f, anchor, onClose, onPreview, drive, onMove, onShare }: DriveFileMenuProps) {
  const setErr = useAppStore((s) => s.setErr);

  const items = useMemo<MenuItem[]>(() => {
    const list: MenuItem[] = [];
    if (!f.is_folder) list.push({ id: "preview", label: "Preview", run: onPreview });
    if (f.web_view_link) {
      list.push({
        id: "open",
        label: "Open in Drive",
        run: () => { openExternal(f.web_view_link).catch((e) => setErr(String(e))); },
      });
    }
    if (!f.is_folder) list.push({ id: "download", label: "Download", run: () => drive.download(f) });
    if (!f.is_folder && f.web_view_link) {
      list.push({ id: "link", label: "Insert link in email", run: () => drive.insertDriveLink(f) });
    }
    list.push({ id: "rename", label: "Rename", separatorBefore: list.length > 0, run: () => drive.rename(f) });
    list.push({ id: "move", label: "Move", run: () => onMove(f) });
    list.push({ id: "share", label: "Share", run: () => onShare(f) });
    list.push({ id: "trash", label: "Move to trash", danger: true, run: () => drive.trash(f) });
    return list;
  }, [f, drive, onPreview, onMove, onShare, setErr]);

  return (
    // Portal events bubble through the React tree; keep them off the card's open handler.
    <span style={{ display: "contents" }} onClick={(e) => e.stopPropagation()}>
      <ContextMenu anchor={anchor} placement="bottom-end" items={items} onClose={onClose} label="File actions" showLead={false} />
    </span>
  );
}
