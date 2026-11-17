import { useCallback, useEffect, useRef, useState } from "react";
import { videoAppHost } from "../../host.js";
import { engineCall } from "../../lib/engine.js";
import { bundleImport, projectsList, settingsGet, storageStatus, SETTINGS_CHANGED, type BundleImportResult } from "../../lib/media.js";
import { baseName, projectInit, projectShow } from "../../lib/project.js";
import { loadRecent } from "./recentProjects.js";

export interface VideoProjectCallbacks {
  onOpen: (dir: string) => void;
  onRecentError: (error: unknown) => void;
  onDropCreateError: (error: unknown) => void;
  onDropImportError: (error: unknown) => void;
  onFilesImported: (count: number) => void;
}
export interface VideoProjectOptions {
  dir: string | null;
  ready: boolean;
  recentRefreshKey: boolean;
  /** Host supplies its fallback project name. */
  fallbackName: string;
  callbacks: VideoProjectCallbacks;
}

/** Project operations without navigation, labels or visual presentation. */
export function useVideoProjects({ dir, ready, recentRefreshKey, fallbackName, callbacks: events }: VideoProjectOptions) {
  const callbacks = useRef(events);
  callbacks.current = events;
  const [projectRoot, setProjectRoot] = useState<string | null>(null);
  const [recents, setRecents] = useState<string[]>([]);
  const [recentInfo, setRecentInfo] = useState<Record<string, { name?: string | null; missing?: boolean }>>({});
  const [dropActive, setDropActive] = useState(false);

  useEffect(() => {
    if (!ready) return;
    let live = true;
    const load = () => void loadRecent()
      .then((dirs) => { if (live) setRecents(dirs.slice(0, 5)); })
      .catch((e) => { if (live) callbacks.current.onRecentError(e); });
    load();
    window.addEventListener(SETTINGS_CHANGED, load);
    return () => { live = false; window.removeEventListener(SETTINGS_CHANGED, load); };
  }, [dir, recentRefreshKey, ready]);

  useEffect(() => {
    let live = true;
    (async () => {
      try {
        const [settings, storage] = await Promise.all([settingsGet(), storageStatus()]);
        if (!live) return;
        const configured = settings.app.project_root;
        setProjectRoot(typeof configured === "string" && configured.trim() ? configured : storage.default_project_root ?? null);
      } catch { /* Drops remain unavailable until the root can be resolved. */ }
    })();
    return () => { live = false; };
  }, []);

  useEffect(() => {
    if (!recents.length) return;
    let live = true;
    projectsList(recents).then((result) => {
      if (!live) return;
      const info: Record<string, { name?: string | null; missing?: boolean }> = {};
      for (const path of recents) info[path] = { missing: true };
      for (const project of result.projects) info[project.dir] = {
        name: project.name, missing: (project.missing_sources ?? 0) > 0 || project.status === "missing_media",
      };
      setRecentInfo(info);
    }).catch(() => undefined);
    return () => { live = false; };
  }, [recents]);

  const openExisting = useCallback(async (path: string) => {
    await projectShow(path);
    callbacks.current.onOpen(path);
  }, []);
  const createProjectAt = useCallback(async (path: string) => {
    await projectInit(path, baseName(path));
    callbacks.current.onOpen(path);
  }, []);
  const importBundleAt = useCallback(async (bundle: string, target: string): Promise<BundleImportResult> => {
    return bundleImport({ bundle, dir: target });
  }, []);
  const openFiles = useCallback(async (paths: string[]) => {
    const first = paths[0];
    if (!first || !projectRoot) return;
    const stem = baseName(first).replace(/\.[^.]+$/, "").replace(/[\\/]/g, "").trim() || fallbackName;
    let created: string;
    try {
      const result = await engineCall<{ dir: string }>("projects.create", { root: projectRoot, name: stem });
      created = result.dir;
    } catch (e) { callbacks.current.onDropCreateError(e); return; }
    try {
      callbacks.current.onOpen(created);
      for (const file of paths) await engineCall("source.add", { dir: created, file });
      callbacks.current.onFilesImported(paths.length);
    } catch (e) { callbacks.current.onDropImportError(e); }
  }, [projectRoot, fallbackName]);

  useEffect(() => {
    let un: (() => void) | undefined;
    void Promise.resolve().then(() => videoAppHost().onDragDrop((event) => {
      if (dir) return;
      if (event.type === "enter" || event.type === "over") setDropActive(true);
      else if (event.type === "leave") setDropActive(false);
      else if (event.type === "drop") { setDropActive(false); void openFiles(event.paths); }
    })).then((unsubscribe) => { un = unsubscribe; }).catch(() => undefined);
    return () => un?.();
  }, [dir, openFiles]);

  return { recents, recentInfo, dropActive, openExisting, createProjectAt, importBundleAt, openFiles };
}
