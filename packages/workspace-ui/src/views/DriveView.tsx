// Drive — the file grid. Mirrors drive.google.com: a breadcrumb/title header
// with type filter + search + grid/list toggle, then a grid (or list) of file
// cards with thumbnails or colored type tiles, each with a hover ⋮ menu.
// Shelf nav + storage live in DriveNav; shared Drive logic in useDrive.
import { memo, useEffect, useState } from "react";
import Icon from "@rightkit/mail-ui/components/Icons";
import { AccountSwitcher } from "@rightkit/mail-ui/components/AccountSwitcher";
import { Button, Dialog, IconButton, Skeleton, SegmentedControl, Input, Select } from "@rightkit/react-shell/react";
import { setPref } from "@rightkit/mail-ui/lib/preferences";
import { useAppStore } from "@rightkit/mail-ui/store/index";
import { useDrive, FOLDER_MIME } from "@rightkit/mail-ui/hooks/useDrive";
import { DriveFileMenu } from "./DriveFileMenu";
import { DrivePreviewModal } from "./DrivePreviewModal";
import { fileGlyph } from "./driveFileGlyph";
import {
  listDriveFiles,
  driveStorageQuota,
  driveMove,
  driveShare,
  driveListPermissions,
  driveRevokePermission,
  type DriveFile,
  type DrivePermission,
} from "@rightkit/mail-ui/lib/ipc";

/** Native button that carries the keyboard/AT semantics of a file card or row;
 *  the mouse click is handled by the card itself. */
const FILE_OPEN_STYLE = {
  display: "flex",
  alignItems: "center",
  gap: 8,
  flex: "1 1 auto",
  minWidth: 0,
  padding: 0,
  border: 0,
  background: "none",
  color: "inherit",
  font: "inherit",
  textAlign: "left",
  cursor: "pointer",
} as const;

const SHELF_LABEL: Record<string, string> = {
  recent: "Recent",
  shared: "Shared with me",
  starred: "Starred",
  trash: "Trash",
};
function DriveViewInner() {
  const drive = useDrive();
  const setErr = useAppStore((s) => s.setErr);
  const showUndo = useAppStore((s) => s.showUndo);
  const setDrivePath = useAppStore((s) => s.setDrivePath);
  const setDriveQuery = useAppStore((s) => s.setDriveQuery);
  const driveFiles = useAppStore((s) => s.driveFiles);
  const setDriveFiles = useAppStore((s) => s.setDriveFiles);
  const driveToken = useAppStore((s) => s.driveToken);
  const setDriveToken = useAppStore((s) => s.setDriveToken);
  const drivePreview = useAppStore((s) => s.drivePreview);
  const setDrivePreview = useAppStore((s) => s.setDrivePreview);
  const driveViewMode = useAppStore((s) => s.driveViewMode);
  const setDriveViewMode = useAppStore((s) => s.setDriveViewMode);
  const setDriveQuota = useAppStore((s) => s.setDriveQuota);
  const driveBusy = useAppStore((s) => s.driveBusy);
  const { activeAccount, driveShelf, drivePath, driveQuery, driveType } = drive;
  // Transient UI state: the open ⋮ menu, the move/share modals, drag-over state.
  const [menuFor, setMenuFor] = useState<string | null>(null);
  const [menuAnchor, setMenuAnchor] = useState<Element | null>(null);
  const openMenuFor = (id: string, anchor: Element) => { setMenuAnchor(anchor); setMenuFor(id); };
  const [dragOver, setDragOver] = useState(false);
  const [moveFile, setMoveFile] = useState<DriveFile | null>(null);
  const [moveFolders, setMoveFolders] = useState<DriveFile[]>([]);
  const [shareFile, setShareFile] = useState<DriveFile | null>(null);
  const [perms, setPerms] = useState<DrivePermission[]>([]);
  const [shareEmail, setShareEmail] = useState("");
  const [shareRole, setShareRole] = useState("reader");
  // Load My Drive root + storage quota on mount / account change.
  useEffect(() => {
    setDrivePath([]);
    setDriveQuery("");
    drive.reload(undefined, "");
    if (activeAccount) driveStorageQuota(activeAccount).then(setDriveQuota).catch(() => {});
    // eslint-disable-next-line react-hooks/exhaustive-deps -- setDrivePath/setDriveQuery/drive.reload/setDriveQuota are store setters or a method on the `drive` object recreated every render; tracking them would reload Drive's root on every render instead of only on account change.
  }, [activeAccount]);
  function openMoveDialog(f: DriveFile) {
    if (!activeAccount) return;
    setMenuFor(null);
    setMoveFile(f);
    setMoveFolders([]);
    listDriveFiles(activeAccount, undefined, undefined, undefined, undefined, FOLDER_MIME)
      .then((l) => setMoveFolders(l.files.filter((x) => x.id !== f.id)))
      .catch((e) => setErr(String(e)));
  }
  function doMove(targetId: string, targetName: string) {
    if (!activeAccount || !moveFile) return;
    const oldParent = driveShelf === "my" ? drive.currentFolderId() ?? "root" : undefined;
    driveMove(activeAccount, moveFile.id, targetId, oldParent)
      .then(() => {
        showUndo(`Moved to ${targetName}`, [], "");
        setMoveFile(null);
        drive.refresh();
      })
      .catch((e) => setErr(String(e)));
  }
  function openShareDialog(f: DriveFile) {
    if (!activeAccount) return;
    setMenuFor(null);
    setShareFile(f);
    setPerms([]);
    setShareEmail("");
    setShareRole("reader");
    driveListPermissions(activeAccount, f.id).then(setPerms).catch((e) => setErr(String(e)));
  }
  function addShare() {
    if (!activeAccount || !shareFile || !shareEmail.trim()) return;
    driveShare(activeAccount, shareFile.id, shareEmail.trim(), shareRole)
      .then(() => {
        showUndo(`Shared with ${shareEmail.trim()} (${shareRole})`, [], "");
        setShareEmail("");
        driveListPermissions(activeAccount, shareFile.id).then(setPerms).catch(() => {});
      })
      .catch((e) => setErr(String(e)));
  }
  function revokeShare(permId: string) {
    if (!activeAccount || !shareFile) return;
    driveRevokePermission(activeAccount, shareFile.id, permId)
      .then(() => {
        setPerms((prev) => prev.filter((p) => p.id !== permId));
        showUndo("Access removed", [], "");
      })
      .catch((e) => setErr(String(e)));
  }
  function openItem(f: DriveFile) {
    if (f.is_folder) drive.openFolder(f);
    else setDrivePreview(f);
  }
  const title = driveShelf === "my" ? null : SHELF_LABEL[driveShelf];
  return (
    <div className="driveview" onClick={() => setMenuFor(null)}>
      <div className="drive-bar">
        {title ? (
          <h2 className="drive-title">{title}</h2>
        ) : (
          <div className="drive-crumbs">
            <Button variant="ghost" className="crumb" onClick={() => drive.breadcrumbTo(-1)}>
              My Drive
            </Button>
            {drivePath.map((p, i) => (
              <span key={p.id} className="crumb-wrap">
                <Icon name="chevron-right" size={14} className="crumb-sep" />
                <Button variant="ghost" className="crumb" onClick={() => drive.breadcrumbTo(i)}>
                  {p.name}
                </Button>
              </span>
            ))}
          </div>
        )}
        <div className="drive-bar-spacer" />
        <Input
          className="drive-search"
          placeholder="Search in Drive…"
          value={driveQuery}
          onChange={(e) => drive.setDriveQuery(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") drive.reload(undefined, driveQuery);
          }}
        />
        <Select
          className="drive-typefilter"
          value={driveType}
          label="Filter by type"
          options={[
            { value: "", label: "All types" },
            { value: FOLDER_MIME, label: "Folders" },
            { value: "document", label: "Docs" },
            { value: "spreadsheet", label: "Sheets" },
            { value: "presentation", label: "Slides" },
            { value: "application/pdf", label: "PDFs" },
            { value: "image/", label: "Images" },
            { value: "video/", label: "Video" },
          ]}
          onChange={drive.setTypeFilter}
        />
        <div className="drive-toggle">
          <SegmentedControl
            label="Drive view"
            value={driveViewMode}
            options={[
              { value: "list", label: <Icon name="list" size={18} />, title: "List view" },
              { value: "grid", label: <Icon name="grid" size={18} />, title: "Grid view" },
            ]}
            onChange={(m) => { setDriveViewMode(m); setPref("driveViewMode", m); }}
          />
        </div>
        <AccountSwitcher />
      </div>
      <div
        className={`drive-scroll ${dragOver ? "drag-over" : ""}`}
        onDragOver={(e) => { e.preventDefault(); if (!dragOver) setDragOver(true); }}
        onDragLeave={(e) => { if (e.currentTarget === e.target) setDragOver(false); }}
        onDrop={(e) => {
          e.preventDefault();
          setDragOver(false);
          if (e.dataTransfer.files?.length) drive.upload(e.dataTransfer.files);
        }}
      >
        {dragOver && (
          <div className="drive-dropzone">
            <Icon name="upload" size={40} />
            <span>Drop files to upload to {driveShelf === "my" && drivePath.length ? drivePath[drivePath.length - 1].name : "My Drive"}</span>
          </div>
        )}
        {driveBusy ? (
          driveViewMode === "grid" ? (
            <div className="drive-grid">
              {Array.from({ length: 8 }).map((_, i) => (
                <Skeleton key={i} height={150} radius={12} />
              ))}
            </div>
          ) : (
            <div className="drive-list">
              {Array.from({ length: 8 }).map((_, i) => (
                <div className="file-row" key={i}><Skeleton height={18} /></div>
              ))}
            </div>
          )
        ) : driveFiles.length === 0 ? (
          <div className="drive-empty">
            <Icon name="drive" size={44} />
            <p className="hint">
              {driveShelf === "trash" ? "Trash is empty." : driveQuery ? "No matching files." : "Nothing here yet."}
            </p>
          </div>
        ) : driveViewMode === "grid" ? (
          <div className="drive-grid">
            {driveFiles.map((f) => {
              const g = fileGlyph(f);
              return (
                <div
                  className={`file-card ${f.is_folder ? "is-folder" : ""}`}
                  key={f.id}
                  onClick={() => openItem(f)}
                  onContextMenu={(e) => { e.preventDefault(); openMenuFor(f.id, e.currentTarget.querySelector(".file-menu-btn") ?? e.currentTarget); }}
                >
                  <div className="file-thumb">
                    {f.thumbnail_link && !f.is_folder ? (
                      <img
                        src={f.thumbnail_link}
                        alt=""
                        loading="lazy"
                        onError={(e) => { (e.currentTarget as HTMLImageElement).style.display = "none"; }}
                      />
                    ) : null}
                    <Icon name={g.icon} size={f.is_folder ? 34 : 30} className="file-glyph"
                      style={g.color ? { color: g.color } : undefined} />
                  </div>
                  <div className="file-foot">
                    <Button variant="ghost" type="button" className="file-open" style={FILE_OPEN_STYLE}>
                      <Icon name={g.icon} size={15} className="file-foot-ic"
                        style={g.color ? { color: g.color } : undefined} />
                      <span className="file-name" title={f.name}>{f.name}</span>
                    </Button>
                    <IconButton label="More" className="file-menu-btn" onClick={(e) => { e.stopPropagation(); if (menuFor === f.id) setMenuFor(null); else openMenuFor(f.id, e.currentTarget); }}>
<Icon name="more" size={16} />
</IconButton>
                  </div>
                  {menuFor === f.id && menuAnchor && <DriveFileMenu f={f} anchor={menuAnchor} onClose={() => setMenuFor(null) }
                    onPreview={() => setDrivePreview(f)} drive={drive}
                    onMove={openMoveDialog} onShare={openShareDialog} />}
                </div>
              );
            })}
          </div>
        ) : (
          <div className="drive-list">
            {driveFiles.map((f) => {
              const g = fileGlyph(f);
              return (
                <div
                  className="file-row"
                  key={f.id}
                  onClick={() => openItem(f)}
                  onContextMenu={(e) => { e.preventDefault(); openMenuFor(f.id, e.currentTarget.querySelector(".file-menu-btn") ?? e.currentTarget); }}
                >
                  <Button variant="ghost" type="button" className="file-open" style={{ ...FILE_OPEN_STYLE, gap: 12 }}>
                    <Icon name={g.icon} size={18} className="file-row-ic"
                      style={g.color ? { color: g.color } : undefined} />
                    <span className="file-row-name" title={f.name}>{f.name}</span>
                    <span className="file-row-date">
                      {f.modified_time ? new Date(f.modified_time).toLocaleDateString() : ""}
                    </span>
                  </Button>
                  <IconButton label="More" className="file-menu-btn" onClick={(e) => { e.stopPropagation(); if (menuFor === f.id) setMenuFor(null); else openMenuFor(f.id, e.currentTarget); }}>
<Icon name="more" size={16} />
</IconButton>
                  {menuFor === f.id && menuAnchor && <DriveFileMenu f={f} anchor={menuAnchor} onClose={() => setMenuFor(null)}
                    onPreview={() => setDrivePreview(f)} drive={drive}
                    onMove={openMoveDialog} onShare={openShareDialog} />}
                </div>
              );
            })}
          </div>
        )}
        {driveToken && !driveBusy && (
          <Button variant="ghost"
            className="label-more drive-loadmore"
            onClick={() => {
              if (!activeAccount) return;
              listDriveFiles(
                activeAccount,
                driveShelf === "my" && drivePath.length ? drivePath[drivePath.length - 1].id : undefined,
                driveQuery || undefined,
                driveToken,
                driveShelf === "my" ? undefined : driveShelf,
                driveType || undefined
              )
                .then((l) => {
                  setDriveFiles((prev) => [...prev, ...l.files]);
                  setDriveToken(l.next_page_token);
                })
                .catch((e) => setErr(String(e)));
            }}
          >
            Load more
          </Button>
        )}
      </div>
      {drivePreview && (
        <DrivePreviewModal drivePreview={drivePreview} setDrivePreview={setDrivePreview} drive={drive} />
      )}
      {moveFile && (
        <Dialog title={`Move “${moveFile.name}” to…`} width={480} onClose={() => setMoveFile(null)}>
            <div className="drive-dialog-list">
              <Button variant="ghost" className="ws-row drive-pick" onClick={() => doMove("root", "My Drive")}>
                <Icon name="drive" size={16} className="ws-ic" />
                <span className="ws-title">My Drive (root)</span>
              </Button>
              {moveFolders.map((folder) => (
                <Button variant="ghost" key={folder.id} className="ws-row drive-pick" onClick={() => doMove(folder.id, folder.name)}>
                  <Icon name="folder" size={16} className="ws-ic" />
                  <span className="ws-title">{folder.name}</span>
                </Button>
              ))}
              {moveFolders.length === 0 && <p className="hint">No folders found.</p>}
            </div>
        </Dialog>
      )}
      {shareFile && (
        <Dialog title={`Share “${shareFile.name}”`} width={480} onClose={() => setShareFile(null)}>
            <div className="share-add">
              <Input
                className="drive-search"
                placeholder="Add person by email…"
                value={shareEmail}
                onChange={(e) => setShareEmail(e.target.value)}
                onKeyDown={(e) => { if (e.key === "Enter") addShare(); }}
              />
              <Select
                className="workspace-select"
                label="Access level"
                value={shareRole}
                options={[
                  { value: "reader", label: "Viewer" },
                  { value: "commenter", label: "Commenter" },
                  { value: "writer", label: "Editor" },
                ]}
                onChange={setShareRole}
              />
              <Button variant="primary" size="sm" onClick={addShare} disabled={!shareEmail.trim()}>Share</Button>
            </div>
            <div className="drive-dialog-list">
              {perms.length === 0 ? (
                <p className="hint">No one else has access yet.</p>
              ) : (
                perms.map((p) => (
                  <div className="ws-row" key={p.id}>
                    <Icon name="people" size={15} className="ws-ic" />
                    <span className="ws-title">{p.email || p.display_name || p.kind || "—"}</span>
                    <span className="ws-sub">{p.role}</span>
                    {p.role !== "owner" && (
                      <Button variant="danger" size="sm" onClick={() => revokeShare(p.id)}>Remove</Button>
                    )}
                  </div>
                ))
              )}
            </div>
        </Dialog>
      )}
    </div>
  );
}
export const DriveView = memo(DriveViewInner);
