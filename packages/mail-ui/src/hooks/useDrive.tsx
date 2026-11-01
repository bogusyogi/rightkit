// Shared Drive logic for DriveNav (the contextual sidebar) and DriveView (the
// file grid). Reads/writes Drive state through the store so both surfaces stay
// in sync (shelf selection in the nav re-lists the grid, etc.). Transient modal
// state (move/share pickers) stays local to DriveView since only it renders them.
import { useAppStore, composeEditorRef } from "../store";
import { showDialog } from "./dialog";
import {
  listDriveFiles,
  driveDownload,
  driveUpload,
  driveCreateFolder,
  driveRename,
  driveTrash,
  type DriveFile,
} from "../lib/ipc";

export const FOLDER_MIME = "application/vnd.google-apps.folder";
export type DriveShelf = "my" | "recent" | "shared" | "starred" | "trash";

export function useDrive() {
  const saveAndScan = useAppStore((s) => s.saveAndScan);
  const openComposeTo = useAppStore((s) => s.openComposeTo);
  const activeAccount = useAppStore((s) => s.activeAccount);
  const setErr = useAppStore((s) => s.setErr);
  const showUndo = useAppStore((s) => s.showUndo);
  const compose = useAppStore((s) => s.compose);
  const driveType = useAppStore((s) => s.driveType);
  const setDriveType = useAppStore((s) => s.setDriveType);
  const driveQuery = useAppStore((s) => s.driveQuery);
  const setDriveQuery = useAppStore((s) => s.setDriveQuery);
  const driveShelf = useAppStore((s) => s.driveShelf);
  const setDriveShelf = useAppStore((s) => s.setDriveShelf);
  const drivePath = useAppStore((s) => s.drivePath);
  const setDrivePath = useAppStore((s) => s.setDrivePath);
  const setDriveToken = useAppStore((s) => s.setDriveToken);
  const setDriveFiles = useAppStore((s) => s.setDriveFiles);
  const setDriveBusy = useAppStore((s) => s.setDriveBusy);

  /** Current Drive folder id for write ops + reload (undefined = My Drive root). */
  function currentFolderId(): string | undefined {
    return driveShelf === "my" && drivePath.length ? drivePath[drivePath.length - 1].id : undefined;
  }

  async function reload(
    folderId: string | undefined,
    query: string,
    shelf: DriveShelf = driveShelf,
    typeFilter = driveType
  ) {
    if (!activeAccount) return;
    setDriveBusy(true);
    try {
      const listing = await listDriveFiles(
        activeAccount,
        folderId,
        query || undefined,
        undefined,
        shelf === "my" ? undefined : shelf,
        typeFilter || undefined
      );
      setDriveFiles(listing.files);
      setDriveToken(listing.next_page_token);
    } catch (e) {
      setErr(String(e));
    } finally {
      setDriveBusy(false);
    }
  }
  /** Switch Drive shelf (My Drive / Recent / Shared / Starred / Trash) — clears folder nav. */
  function selectShelf(shelf: DriveShelf) {
    setDriveShelf(shelf);
    setDrivePath([]);
    setDriveQuery("");
    reload(undefined, "", shelf, driveType);
  }
  function setTypeFilter(t: string) {
    setDriveType(t);
    reload(currentFolderId(), driveQuery, driveShelf, t);
  }
  function openFolder(f: DriveFile) {
    setDriveShelf("my");
    setDrivePath((p) => [...p, { id: f.id, name: f.name }]);
    setDriveQuery("");
    reload(f.id, "", "my", driveType);
  }
  function breadcrumbTo(idx: number) {
    const next = idx < 0 ? [] : drivePath.slice(0, idx + 1);
    setDrivePath(next);
    setDriveQuery("");
    reload(next.length ? next[next.length - 1].id : undefined, "", "my", driveType);
  }
  function refresh() {
    reload(currentFolderId(), driveQuery, driveShelf, driveType);
  }
  function download(f: DriveFile) {
    if (!activeAccount) return;
    saveAndScan(f.name, () => driveDownload(activeAccount, f.id, f.mime_type, f.name));
  }
  async function newFolder() {
    if (!activeAccount) return;
    const name = await showDialog({ kind: "prompt", message: "New folder name" });
    if (!name || !name.trim()) return;
    driveCreateFolder(activeAccount, name.trim(), currentFolderId())
      .then(() => {
        showUndo("Folder created", [], "");
        refresh();
      })
      .catch((e) => setErr(String(e)));
  }
  /** Upload one or more files into the current folder, then refresh. */
  async function upload(files: FileList | File[]) {
    if (!activeAccount) return;
    const list = Array.from(files);
    if (list.length === 0) return;
    const parent = currentFolderId();
    setDriveBusy(true);
    let ok = 0;
    for (const file of list) {
      try {
        const data = Array.from(new Uint8Array(await file.arrayBuffer()));
        await driveUpload(activeAccount, file.name, file.type, data, parent);
        ok++;
      } catch (e) {
        setErr(String(e));
        break;
      }
    }
    setDriveBusy(false);
    if (ok > 0) {
      showUndo(`Uploaded ${ok} file${ok > 1 ? "s" : ""}`, [], "");
      refresh();
    }
  }
  async function rename(f: DriveFile) {
    if (!activeAccount) return;
    const name = await showDialog({ kind: "prompt", message: "Rename to", initial: f.name });
    if (!name || !name.trim() || name.trim() === f.name) return;
    driveRename(activeAccount, f.id, name.trim())
      .then(() => {
        showUndo("Renamed", [], "");
        refresh();
      })
      .catch((e) => setErr(String(e)));
  }
  async function trash(f: DriveFile) {
    if (!activeAccount) return;
    if ((await showDialog({ kind: "confirm", message: `Move "${f.name}" to Drive trash? You can restore it from Google Drive.` })) !== "ok")
      return;
    driveTrash(activeAccount, f.id)
      .then(() => {
        showUndo("Moved to Drive trash", [], "");
        refresh();
      })
      .catch((e) => setErr(String(e)));
  }
  /** Insert a Drive file's share link into the compose body (open compose if needed). */
  function insertDriveLink(f: DriveFile) {
    const link = f.web_view_link;
    if (!link) {
      setErr("That item has no shareable link.");
      return;
    }
    const html = `<a href="${link}">${f.name}</a>`;
    if (compose) {
      composeEditorRef.current?.insertHTML(`${html}&nbsp;`);
      showUndo(`Linked ${f.name}`, [], "");
    } else {
      openComposeTo("", "", `${html}<br><br>`);
      showUndo(`Linked ${f.name} into a new email`, [], "");
    }
  }

  return {
    activeAccount, driveShelf, drivePath, driveQuery, driveType,
    setDriveQuery,
    currentFolderId, reload, selectShelf, setTypeFilter, openFolder,
    breadcrumbTo, refresh, download, upload, newFolder, rename, trash, insertDriveLink,
  };
}
