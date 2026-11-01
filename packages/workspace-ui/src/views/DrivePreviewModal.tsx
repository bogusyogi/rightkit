import { Button, Dialog } from "@rightkit/react-shell/react";
import Icon from "@rightkit/mail-ui/components/Icons";
import { useDrive } from "@rightkit/mail-ui/hooks/useDrive";
import { openExternal, type DriveFile } from "@rightkit/mail-ui/lib/ipc";
import { formatBytes } from "@rightkit/settings-ui/core";
import { useAppStore } from "@rightkit/mail-ui/store/index";
import { fileGlyph } from "./driveFileGlyph.js";

type DrivePreviewModalProps = {
  drivePreview: DriveFile;
  setDrivePreview: (f: DriveFile | null) => void;
  drive: ReturnType<typeof useDrive>;
};

export function DrivePreviewModal({ drivePreview, setDrivePreview, drive }: DrivePreviewModalProps) {
  const setErr = useAppStore((s) => s.setErr);

  return (
    <Dialog title={drivePreview.name} width={520} onClose={() => setDrivePreview(null)}>
        {drivePreview.thumbnail_link ? (
          <img
            className="dp-thumb"
            src={drivePreview.thumbnail_link}
            alt=""
            onError={(e) => { (e.currentTarget as HTMLImageElement).style.display = "none"; }}
          />
        ) : (
          <div className="dp-noimg">
            <Icon name={fileGlyph(drivePreview).icon} size={48} />
            <span className="hint">No preview available</span>
          </div>
        )}
        <div className="dp-meta hint">
          {drivePreview.mime_type}
          {drivePreview.size ? ` · ${formatBytes(Number(drivePreview.size))}` : ""}
          {drivePreview.modified_time ? ` · ${new Date(drivePreview.modified_time).toLocaleString()}` : ""}
        </div>
        <div className="dp-actions">
          {drivePreview.web_view_link && (
            <Button variant="primary" onClick={() => openExternal(drivePreview.web_view_link).catch((e) => setErr(String(e)))}>
              Open in Drive
            </Button>
          )}
          <Button variant="ghost" size="sm" onClick={() => drive.download(drivePreview)}>Download</Button>
          <Button variant="ghost" size="sm" onClick={() => { drive.insertDriveLink(drivePreview); setDrivePreview(null); }}>
            Insert link in email
          </Button>
        </div>
    </Dialog>
  );
}
