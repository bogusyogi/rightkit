import { useCallback, useEffect, useRef, useState } from "react";
import { errorMessage } from "../../lib/engine.js";
import { mediaScopeAllow, rendersList, type BundleImportResult } from "../../lib/media.js";
import { baseName, projectShow, type ProjectShow } from "../../lib/project.js";
import { rememberProject } from "./recentProjects.js";
import type { useProgress } from "./useProgress.js";

/** Project state and effects; navigation and error presentation stay with the host. */
export function useVideoWorkspace(progressController: ReturnType<typeof useProgress>) {
  const { progress, reset } = progressController;
  const [selection, setSelection] = useState<{ dir: string } | null>(null);
  const currentSelection = useRef(selection);
  const dir = selection?.dir ?? null;
  const [show, setShow] = useState<ProjectShow | null>(null);
  const [exportsCount, setExportsCount] = useState(0);
  const [jobError, setJobError] = useState<string | null>(null);
  const [output, setOutput] = useState<string | null>(null);
  const [importMissing, setImportMissing] = useState<string[] | null>(null);
  const [importUnverified, setImportUnverified] = useState<string[] | null>(null);

  useEffect(() => {
    if (!dir) return;
    void mediaScopeAllow([dir, `${dir}/media`, `${dir}/renders`, `${dir}/variants`, `${dir}/.cache`], true).catch(() => undefined);
  }, [dir]);
  const refresh = useCallback(async () => {
    if (!selection || currentSelection.current !== selection) return;
    let result: ProjectShow;
    try {
      result = await projectShow(selection.dir);
    } catch (error) {
      if (currentSelection.current !== selection) return;
      throw error;
    }
    if (currentSelection.current !== selection) return;
    setShow(result);
    rendersList(selection.dir)
      .then((r) => {
        if (currentSelection.current === selection) setExportsCount(r.renders.filter((x) => x.name.endsWith(".mp4")).length);
      })
      .catch(() => undefined);
  }, [selection]);
  useEffect(() => {
    if (!selection) return;
    let cancelled = false;
    const current = () => !cancelled && currentSelection.current === selection;
    projectShow(selection.dir).then((s) => current() && setShow(s)).catch((e) => current() && setJobError(errorMessage(e)));
    rendersList(selection.dir).then((r) => current() && setExportsCount(r.renders.filter((x) => x.name.endsWith(".mp4")).length)).catch(() => undefined);
    return () => { cancelled = true; };
  }, [selection]);
  useEffect(() => {
    if (progress.output) setOutput(progress.output);
    if (progress.done || progress.error) void refresh().catch(() => undefined);
  }, [progress.output, progress.done, progress.error, refresh]);

  const open = useCallback((next: string, navigate: () => void, clearRecovery: () => void, onRecentError: (e: unknown) => void) => {
    const nextSelection = { dir: next };
    currentSelection.current = nextSelection;
    setShow(null); setExportsCount(0); setOutput(null); setJobError(null);
    setImportMissing(null); setImportUnverified(null);
    reset(); navigate(); clearRecovery(); setSelection(nextSelection);
    void rememberProject(next).catch(onRecentError);
  }, [reset]);
  const close = useCallback((navigate: () => void) => {
    currentSelection.current = null;
    setSelection(null); setShow(null); setExportsCount(0); setImportMissing(null); setImportUnverified(null); navigate();
  }, []);
  const applyBundleReport = useCallback((report: BundleImportResult) => {
    const missing = [
      ...report.missing_media.map((m) => m.name ?? baseName(m.path)),
      ...report.missing_assets.map((path) => baseName(path)),
      ...(report.missing_generated_sources ?? []).map((g) => baseName(g.path)),
    ];
    const unverified = [
      ...(report.unverified_assets ?? []).map((path) => baseName(path)),
      ...(report.unverified_generated_sources ?? []).map((g) => baseName(g.path)),
    ];
    if (unverified.length) setImportUnverified(unverified);
    if (missing.length) setImportMissing(missing);
    return { missing, unverified };
  }, []);
  return { dir, show, exportsCount, jobError, setJobError, output, setOutput, importMissing, setImportMissing, importUnverified, setImportUnverified, refresh, open, close, applyBundleReport };
}

/** Publish busy state while mounted and clear it on change or unmount. */
export function useVideoUpdateBusy(dir: string | null, assembling: boolean, rendering: boolean, running: boolean, setBusy: (busy: boolean) => void): void {
  useEffect(() => { setBusy(dir !== null || assembling || rendering || running); return () => setBusy(false); }, [dir, assembling, rendering, running, setBusy]);
}
