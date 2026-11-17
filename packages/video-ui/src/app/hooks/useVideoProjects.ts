import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { videoAppHost } from "../../host.js";
import { engineCall } from "../../lib/engine.js";
import { bundleImport, projectsList, settingsGet, storageStatus, SETTINGS_CHANGED, type BundleImportResult } from "../../lib/media.js";
import { baseName, projectInit, projectShow } from "../../lib/project.js";
import { pickFolder } from "../lib/dialog.js";
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
  captureSelection?: () => () => boolean;
}

/** Project operations without navigation, labels or visual presentation. */
export function useVideoProjects({ dir, ready, recentRefreshKey, fallbackName, callbacks: events, captureSelection }: VideoProjectOptions) {
  const callbacks = useRef(events);
  callbacks.current = events;
  const [projectRoot, setProjectRoot] = useState<string | null>(null);
  const [recents, setRecents] = useState<string[]>([]);
  const [recentInfo, setRecentInfo] = useState<Record<string, { name?: string | null; missing?: boolean }>>({});
  const [dropActive, setDropActive] = useState(false);
  const actionEpoch = useRef(0);
  const mounted = useRef(false);
  const bundlePending = useRef(false);
  const dialogPending = useRef(false);
  const lastOpened = useRef<{ epoch: number; stillCurrent: () => boolean } | null>(null);
  useLayoutEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; actionEpoch.current += 1; };
  }, []);
  useLayoutEffect(() => { if (!captureSelection) actionEpoch.current += 1; }, [dir, captureSelection]);

  const capturePicker = useCallback(() => {
    const epoch = actionEpoch.current;
    const stillCurrent = captureSelection?.();
    return () => {
      if (!mounted.current || actionEpoch.current !== epoch) return false;
      if (!stillCurrent || stillCurrent()) return true;
      // Older work completing during the dialog must not discard a new positive choice.
      const opened = lastOpened.current;
      return opened?.epoch === epoch && opened.stillCurrent();
    };
  }, [captureSelection]);
  const openValidated = useCallback((path: string, epoch: number) => {
    callbacks.current.onOpen(path);
    const stillCurrent = captureSelection?.();
    if (stillCurrent) lastOpened.current = { epoch, stillCurrent };
  }, [captureSelection]);

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
    const epoch = ++actionEpoch.current;
    const sameSelection = captureSelection?.() ?? (() => true);
    await projectShow(path);
    if (mounted.current && actionEpoch.current === epoch && sameSelection()) openValidated(path, epoch);
  }, [captureSelection, openValidated]);
  const createProjectAt = useCallback(async (path: string) => {
    const epoch = ++actionEpoch.current;
    const sameSelection = captureSelection?.() ?? (() => true);
    await projectInit(path, baseName(path));
    if (mounted.current && actionEpoch.current === epoch && sameSelection()) openValidated(path, epoch);
  }, [captureSelection, openValidated]);
  const importBundleAt = useCallback(async (bundle: string, target: string): Promise<BundleImportResult> => {
    return bundleImport({ bundle, dir: target });
  }, []);
  const openFiles = useCallback(async (paths: string[]) => {
    const first = paths[0];
    if (!first || !projectRoot) return;
    const epoch = ++actionEpoch.current;
    const sameSelection = captureSelection?.() ?? (() => true);
    const stem = baseName(first).replace(/\.[^.]+$/, "").replace(/[\\/]/g, "").trim() || fallbackName;
    let created: string;
    try {
      const result = await engineCall<{ dir: string }>("projects.create", { root: projectRoot, name: stem });
      created = result.dir;
    } catch (e) { if (mounted.current && actionEpoch.current === epoch && sameSelection()) callbacks.current.onDropCreateError(e); return; }
    const showResult = mounted.current && actionEpoch.current === epoch && sameSelection();
    if (showResult) openValidated(created, epoch);
    const stillOpened = captureSelection?.();
    const shouldNotify = () => showResult && mounted.current && (!stillOpened || stillOpened());
    try {
      for (const file of paths) await engineCall("source.add", { dir: created, file });
      if (shouldNotify()) callbacks.current.onFilesImported(paths.length);
    } catch (e) { if (shouldNotify()) callbacks.current.onDropImportError(e); }
  }, [projectRoot, fallbackName, captureSelection, openValidated]);

  const pickProject = useCallback(async (title: string, kind: "open" | "create"): Promise<
    | { status: "cancelled" | "stale" | "busy" }
    | { status: "error"; error: unknown }
    | { status: "opened" }
  > => {
    if (!mounted.current) return { status: "stale" };
    if (dialogPending.current) return { status: "busy" };
    const current = capturePicker();
    let path: string | null;
    dialogPending.current = true;
    try {
      path = await pickFolder(title);
    } catch (error) {
      return current() ? { status: "error", error } : { status: "stale" };
    } finally {
      dialogPending.current = false;
    }
    if (!current()) return { status: "stale" };
    if (!path) return { status: "cancelled" };
    const epoch = ++actionEpoch.current;
    const sameSelection = captureSelection?.();
    const actionCurrent = () => mounted.current && actionEpoch.current === epoch && (!sameSelection || sameSelection());
    try {
      if (kind === "open") await projectShow(path);
      else await projectInit(path, baseName(path));
      if (!actionCurrent()) return { status: "stale" };
      openValidated(path, epoch);
      return { status: "opened" };
    } catch (error) {
      return actionCurrent() ? { status: "error", error } : { status: "stale" };
    }
  }, [captureSelection, capturePicker, openValidated]);
  const pickOpenProject = useCallback((title: string) => pickProject(title, "open"), [pickProject]);
  const pickCreateProject = useCallback((title: string) => pickProject(title, "create"), [pickProject]);

  const importBundle = useCallback(async ({ bundleTitle, targetTitle, applyReport }: {
    bundleTitle: string;
    targetTitle: string;
    applyReport: (report: BundleImportResult) => { missing: string[]; unverified: string[] };
  }): Promise<
    | { status: "cancelled" | "stale" | "busy" }
    | { status: "import-error" | "open-error"; error: unknown }
    | { status: "imported"; missing: string[]; unverified: string[] }
  > => {
    if (!mounted.current) return { status: "stale" };
    if (bundlePending.current || dialogPending.current) return { status: "busy" };
    const pickingCurrent = capturePicker();
    let bundle: string | null;
    let target: string | null;
    dialogPending.current = true;
    try {
      bundle = await pickFolder(bundleTitle);
      if (!pickingCurrent()) return { status: "stale" };
      if (!bundle) return { status: "cancelled" };
      target = await pickFolder(targetTitle);
      if (!pickingCurrent()) return { status: "stale" };
      if (!target) return { status: "cancelled" };
    } catch (error) {
      return pickingCurrent() ? { status: "import-error", error } : { status: "stale" };
    } finally {
      dialogPending.current = false;
    }
    if (!pickingCurrent()) return { status: "stale" };
    bundlePending.current = true;
    const epoch = ++actionEpoch.current;
    const sameSelection = captureSelection?.();
    const current = () => mounted.current && actionEpoch.current === epoch && (!sameSelection || sameSelection());
    try {
      let report: BundleImportResult;
      try { report = await bundleImport({ bundle, dir: target }); }
      catch (error) { return current() ? { status: "import-error", error } : { status: "stale" }; }
      if (!current()) return { status: "stale" };
      try { await projectShow(report.dir); }
      catch (error) { return current() ? { status: "open-error", error } : { status: "stale" }; }
      if (!current()) return { status: "stale" };
      openValidated(report.dir, epoch);
      const warnings = applyReport(report);
      return { status: "imported", ...warnings };
    } finally {
      bundlePending.current = false;
    }
  }, [captureSelection, capturePicker, openValidated]);

  useEffect(() => {
    let un: (() => void) | undefined;
    let live = true;
    void Promise.resolve().then(() => videoAppHost().onDragDrop((event) => {
      if (!live || dir) return;
      if (event.type === "enter" || event.type === "over") setDropActive(true);
      else if (event.type === "leave") setDropActive(false);
      else if (event.type === "drop") { setDropActive(false); void openFiles(event.paths); }
    })).then((unsubscribe) => { if (live) un = unsubscribe; else unsubscribe(); }).catch(() => undefined);
    return () => { live = false; un?.(); };
  }, [dir, openFiles]);

  return { recents, recentInfo, dropActive, openExisting, createProjectAt, pickOpenProject, pickCreateProject, importBundleAt, importBundle, openFiles };
}
