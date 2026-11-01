import { useCallback, useState } from "react";
import {
  clearRecent,
  listRecent,
  listPins,
  togglePin as dbTogglePin,
  togglePinDir as dbTogglePinDir,
  isPinnedPath,
  type FileEntry,
  type OpenFileEntry,
  type PinEntry,
} from "../state/library";
import { baseName, formatFromPath, isUntitled, type LoadedDocument } from "@rightkit/document-core/documents/types";
import { isWebTabKey } from "@rightkit/document-core/documents/web/webUrl";

/**
 * Recents + pinned-files library state. Owns the recents list and the set of pinned paths, the
 * refresh against the DB, pin toggling, and syncing a single path's pinned flag (used right after
 * opening a file). Extracted from App so the shell isn't carrying library plumbing inline.
 *
 * Since the explorer sidebar, the DB also pins directories: `pinnedEntries` is the full pin list
 * (dirs + files, in stored order) while `pinnedPaths` stays the flat Set used for pin-state lookup.
 */
export function useLibrary(openDocs: LoadedDocument[]) {
  const [recents, setRecents] = useState<FileEntry[]>([]);
  const [pinnedPaths, setPinnedPaths] = useState<Set<string>>(() => new Set());
  const [pinnedEntries, setPinnedEntries] = useState<PinEntry[]>([]);

  const refreshLibrary = useCallback(async () => {
    const [recentRows, pinRows] = await Promise.all([listRecent(), listPins()]);
    setRecents(recentRows);
    setPinnedEntries(pinRows);
    // Keep the flat path set in sync — but only for pins the DB knows about. Per-doc syncPinned
    // calls (which reflect is_pinned_path right after open) merge into this set too, so a dir pin
    // set here isn't lost: rebuild the set as union(db pins, existing non-DB-synced entries)? No —
    // DB is the source of truth for pins; syncPinned only mirrors DB state anyway.
    setPinnedPaths(new Set(pinRows.map((p) => p.path)));
  }, []);

  const setPinned = useCallback((path: string, pinned: boolean) => {
    setPinnedPaths((prev) => {
      if (prev.has(path) === pinned) return prev;
      const next = new Set(prev);
      if (pinned) next.add(path);
      else next.delete(path);
      return next;
    });
  }, []);

  /** Reflect the DB's pinned state for one path into local state (called after open). */
  const syncPinned = useCallback(
    async (path: string) => {
      setPinned(path, await isPinnedPath(path));
    },
    [setPinned],
  );

  const onTogglePin = useCallback(
    async (path: string) => {
      if (isUntitled(path) || isWebTabKey(path)) return; // can't pin an unsaved/virtual doc
      const doc = openDocs.find((d) => d.path === path);
      const entry: OpenFileEntry = {
        path,
        displayName: baseName(path),
        format: doc?.format ?? formatFromPath(path),
      };
      const pinnedNow = await dbTogglePin(entry);
      setPinned(path, pinnedNow);
      await refreshLibrary();
    },
    [openDocs, refreshLibrary, setPinned],
  );

  const onTogglePinDir = useCallback(
    async (path: string, displayName: string) => {
      const pinnedNow = await dbTogglePinDir(path, displayName);
      setPinned(path, pinnedNow);
      await refreshLibrary();
    },
    [refreshLibrary, setPinned],
  );

  const onClearRecents = useCallback(async () => {
    await clearRecent();
    setRecents([]);
  }, []);

  const onHistoryPurged = useCallback(() => {
    setRecents([]);
    setPinnedEntries([]);
    setPinnedPaths(new Set());
  }, []);

  return {
    onHistoryPurged,
    recents,
    pinnedPaths,
    pinnedEntries,
    refreshLibrary,
    syncPinned,
    onTogglePin,
    onTogglePinDir,
    onClearRecents,
  };
}
