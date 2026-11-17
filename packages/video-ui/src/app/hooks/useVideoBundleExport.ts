import { useLayoutEffect, useRef, useState, useSyncExternalStore } from "react";
import { errorMessage, onEngineEvent, revealPath } from "../../lib/engine.js";
import { bundleExport, type BundleExportResult, type BundleMedia } from "../../lib/media.js";
import { pickFolder } from "../lib/dialog.js";

type BundleState = { busy: boolean; progress: { completed: number; total: number } | null; error: string | null; result: BundleExportResult | null };
const records = new Map<string, BundleState>();
const listeners = new Set<() => void>();
let snapshot: ReadonlyMap<string, BundleState> = new Map();
const subscribe = (listener: () => void) => { listeners.add(listener); return () => { listeners.delete(listener); }; };
function publish(dir: string, value: BundleState) { records.set(dir, value); snapshot = new Map(records); for (const listener of listeners) listener(); }

export function useVideoBundleExport({ dir }: { dir: string }) {
  const [media, setMediaState] = useState<BundleMedia>("collected");
  const [folder, setFolder] = useState<string | null>(null);
  const [localError, setLocalError] = useState<string | null>(null);
  const [hiddenResult, setHiddenResult] = useState(false);
  const scope = useRef<object | null>(null);
  const choosing = useRef(false);
  const state = useSyncExternalStore(subscribe, () => snapshot, () => snapshot).get(dir);
  useLayoutEffect(() => {
    const token = {}; scope.current = token; setFolder(null); setLocalError(null); setHiddenResult(false); setMediaState("collected");
    return () => { if (scope.current === token) scope.current = null; };
  }, [dir]);
  const setMedia = (value: BundleMedia) => { if (!records.get(dir)?.busy) { setMediaState(value); setHiddenResult(true); } };
  const choose = async (title: string) => {
    const token = scope.current;
    if (!token || choosing.current || records.get(dir)?.busy) return false;
    choosing.current = true;
    try {
      const path = await pickFolder(title);
      if (scope.current !== token || !path || records.get(dir)?.busy) return false;
      setFolder(path); setHiddenResult(true); setLocalError(null); return true;
    } catch (e) { if (scope.current === token) setLocalError(errorMessage(e)); return false; }
    finally { choosing.current = false; }
  };
  const run = async () => {
    if (!scope.current || !folder || records.get(dir)?.busy) return false;
    const tag = `bundle-export-${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}`;
    let current: BundleState = { busy: true, progress: null, error: null, result: null };
    setHiddenResult(false); setLocalError(null); publish(dir, current);
    let off: (() => void) | undefined;
    try {
      try { off = await onEngineEvent((frame) => {
        if (frame.tag !== tag || frame.event.event !== "bundle.progress") return;
        const event = frame.event;
        if (typeof event.completed !== "number" || typeof event.total !== "number") return;
        current = { ...current, progress: { completed: event.completed, total: event.total } }; publish(dir, current);
      }); } catch { /* A missing progress feed does not prevent the export. */ }
      const result = await bundleExport(dir, { out: folder, media }, tag);
      current = { ...current, result };
      return true;
    } catch (e) { current = { ...current, error: errorMessage(e) }; return false; }
    finally {
      try { off?.(); }
      catch { /* The completed export is independent of progress cleanup. */ }
      finally { publish(dir, { ...current, busy: false, progress: null }); }
    }
  };
  const reveal = async () => { if (state?.result) await revealPath(state.result.out); };
  return { media, setMedia, folder, busy: state?.busy ?? false, progress: state?.progress ?? null, error: localError ?? (hiddenResult ? null : state?.error ?? null), result: hiddenResult ? null : state?.result ?? null, choose, run, reveal };
}
