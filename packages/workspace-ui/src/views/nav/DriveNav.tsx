// Drive's contextual sidebar: the "New" button + shelf nav (My Drive / Shared /
// Recent / Starred / Trash) + storage meter. Mirrors drive.google.com's left
// rail. Shares Drive state with the file grid via useDrive, so picking a shelf
// here re-lists the grid.
import { useRef, useState } from "react";
import { Button, ContextMenu, NavList, Input } from "@rightkit/react-shell/react";
import { Icon } from "../../icons.js";
import { formatBytes } from "@rightkit/settings-ui/core";
import { useDrive } from "../../hooks/useDrive.js";
import type { DriveShelf } from "../../client.js";
import type { WorkspaceIconName } from "../../icons.js";
import { useWorkspaceState } from "../../host.js";

const SHELVES: { key: DriveShelf; label: string; icon: WorkspaceIconName }[] = [
  { key: "my", label: "My Drive", icon: "drive" },
  { key: "shared", label: "Shared with me", icon: "people" },
  { key: "recent", label: "Recent", icon: "clock" },
  { key: "starred", label: "Starred", icon: "star" },
  { key: "trash", label: "Trash", icon: "trash" },
];

export function DriveNav() {
  const drive = useDrive();
  const quota = useWorkspaceState((s) => s.driveQuota);
  const [newAnchor, setNewAnchor] = useState<Element | null>(null);
  const fileRef = useRef<HTMLInputElement>(null);

  const used = quota ? Number(quota.usage) : 0;
  const limit = quota && quota.limit ? Number(quota.limit) : 0;
  const pct = limit > 0 ? Math.min(100, Math.round((used / limit) * 100)) : 0;

  return (
    <div className="appnav">
      <div className="nav-new-wrap">
        <Button variant="primary" className="nav-new" onClick={(e) => setNewAnchor((a) => (a ? null : e.currentTarget))} title="New">
          <Icon name="plus" size={20} />
          <span>New</span>
        </Button>
        {newAnchor && (
          <ContextMenu
            anchor={newAnchor}
            placement="bottom-start"
            label="New"
            showLead
            onClose={() => setNewAnchor(null)}
            items={[
              { id: "folder", label: "New folder", icon: <Icon name="folder" size={15} />, run: () => drive.newFolder() },
              { id: "upload", label: "Upload files", icon: <Icon name="upload" size={15} />, run: () => fileRef.current?.click() },
            ]}
          />
        )}
        <Input
          ref={fileRef}
          type="file"
          multiple
          style={{ display: "none" }}
          onChange={(e) => {
            if (e.target.files && e.target.files.length) drive.upload(e.target.files);
            e.target.value = "";
          }}
        />
      </div>

      <NavList
        label="Drive shelves"
        groups={[
          {
            items: SHELVES.map((s) => ({
              id: s.key,
              label: s.label,
              ariaLabel: s.label,
              icon: <Icon name={s.icon} size={18} className="f-ic" />,
            })),
          },
        ]}
        activeId={drive.driveShelf}
        onNavigate={(id) => drive.selectShelf(id as DriveShelf)}
        itemProps={(_item, { active }) => ({ className: `nav-shelf${active ? " active" : ""}` })}
      />

      {quota && (
        <div className="nav-storage">
          <div className="storage-bar">
            <span className="storage-fill" style={{ width: `${pct}%` }} />
          </div>
          <div className="storage-text">
            {formatBytes(used)} {limit > 0 ? `of ${formatBytes(limit)} used` : "used"}
          </div>
        </div>
      )}
    </div>
  );
}
