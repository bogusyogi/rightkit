import { useCallback, useEffect, useLayoutEffect, useRef, useState, useSyncExternalStore } from "react";
import { assetUrl } from "../../lib/asset.js";
import { errorCode, errorMessage, onEngineEvent } from "../../lib/engine.js";
import {
  batchCover, batchCreate, batchList, batchRun, batchStatus, recipeCancel, SETTINGS_CHANGED, variantsList,
  type BatchCover, type BatchDocument, type BatchOutputRow, type BatchPreset, type BatchRunResult,
} from "../../lib/media.js";

type Run = {
  job: string;
  id: string;
  active: boolean;
  version: number;
  result?: BatchRunResult;
  error?: string;
  cancelled?: boolean;
  cancelPending?: boolean;
  cancelSending?: boolean;
  cancelDeclined?: boolean;
  cancelError?: string;
};
type ActiveRun = Run & { registered: boolean; sendingCancel: boolean; cancel?: () => void };
type ProjectState = { revision: number; write?: "create" | "cover"; run?: Run };
const emptyProject: ProjectState = { revision: 0 };
const projects = new Map<string, ProjectState>();
const activeRuns = new Map<string, ActiveRun>();
const subscriptions = new Map<string, Set<() => void>>();
const currentProject = (dir: string) => projects.get(dir) ?? emptyProject;

function subscribe(dir: string, listener: () => void) {
  const listeners = subscriptions.get(dir) ?? new Set();
  listeners.add(listener);
  subscriptions.set(dir, listeners);
  return () => {
    listeners.delete(listener);
    if (!listeners.size) subscriptions.delete(dir);
  };
}

function publish(dir: string, changes: Partial<ProjectState>) {
  projects.set(dir, { ...currentProject(dir), ...changes });
  for (const listener of subscriptions.get(dir) ?? []) listener();
}

const presetOrder: readonly BatchPreset[] = ["youtube", "reels", "tiktok", "square"];

function toggle<T>(old: Set<T>, value: T, enabled: boolean) {
  const next = new Set(old);
  if (enabled) next.add(value);
  else next.delete(value);
  return next;
}

function coverUrl(dir: string, cover: BatchCover) {
  const separator = dir.includes("\\") ? "\\" : "/";
  const path = dir.replace(/[\\/]+$/, "") + separator + cover.path.split("/").join(separator);
  return assetUrl(path) + "?v=" + cover.blake3;
}

type Selection = {
  dir: string;
  id: string;
  epoch: number;
  listRead: number;
  statusRead: number;
};
type Detail = { doc: BatchDocument | null; reading: boolean; drift?: string[]; compareError?: string; readError?: string };
const emptyDetail: Detail = { doc: null, reading: false };

export function useVideoBatchDelivery({ dir, onChanged }: { dir: string; onChanged?: (dir: string) => void | Promise<void> }) {
  const [variants, setVariants] = useState<string[]>([]);
  const [presets, setPresets] = useState<Set<BatchPreset>>(() => new Set(["youtube"]));
  const [timelines, setTimelines] = useState<Set<string>>(() => new Set([""]));
  const [batches, setBatches] = useState<{ id: string; counts?: Record<string, number> }[]>([]);
  const [batchId, setBatchId] = useState("");
  const [detail, setDetail] = useState<Detail>(emptyDetail);
  const [coverAt, setCoverAt] = useState<Record<string, string>>({});
  const [coverError, setCoverError] = useState<string>();
  const [actionError, setActionError] = useState<string>();
  const [listError, setListError] = useState<string>();
  const selection = useRef<Selection | null>(null);
  const eventRefresh = useRef<{ token: Selection; epoch: number; queued: boolean } | undefined>(undefined);
  const snapshot = useSyncExternalStore((listener) => subscribe(dir, listener), () => currentProject(dir), () => emptyProject);
  const { run: lastRun, write: writing, revision } = snapshot;
  const running = lastRun?.active ? lastRun.job : null;
  const selectedRun = lastRun?.id === batchId ? lastRun : undefined;

  useLayoutEffect(() => {
    const token = { dir, id: "", epoch: 0, listRead: 0, statusRead: 0 };
    selection.current = token;
    setPresets(new Set(["youtube"]));
    setTimelines(new Set([""]));
    setVariants([]);
    setBatches([]);
    setBatchId("");
    setDetail(emptyDetail);
    setCoverAt({});
    setCoverError(undefined);
    setActionError(undefined);
    setListError(undefined);
    return () => { if (selection.current === token) selection.current = null; };
  }, [dir]);

  const refreshList = useCallback(async () => {
    const token = selection.current;
    if (!token || token.dir !== dir) return;
    const generation = ++token.listRead;
    const isCurrent = () => selection.current === token && token.listRead === generation;
    try {
      const [list, saved] = await Promise.all([batchList(dir), variantsList(dir)]);
      if (!isCurrent()) return;
      setBatches(list.batches);
      setVariants(saved.variants.filter((item) => !item.error).map((item) => item.name));
      setListError(undefined);
    } catch (error) {
      if (isCurrent()) setListError(errorMessage(error));
    }
  }, [dir]);

  const refreshSelected = useCallback(async () => {
    const token = selection.current;
    if (!token || token.dir !== dir || !token.id) return;
    const id = token.id;
    const generation = ++token.statusRead;
    const isCurrent = () => selection.current === token && token.id === id && token.statusRead === generation;
    setDetail((old) => ({ ...old, reading: true }));
    try {
      const status = await batchStatus(dir, id);
      if (!isCurrent()) return;
      setDetail({
        doc: status,
        reading: false,
        drift: status.finishing_matches === false && !status.finishing_error ? status.finishing_differs : undefined,
        compareError: status.finishing_error,
      });
    } catch (error) {
      if (!isCurrent()) return;
      setDetail((old) => ({ doc: old.doc, reading: false, readError: errorMessage(error) }));
    }
  }, [dir]);

  useEffect(() => {
    if (writing) return;
    void refreshList();
    void refreshSelected();
  }, [revision, writing, refreshList, refreshSelected]);

  useEffect(() => {
    const refresh = () => { void refreshSelected(); };
    const visible = () => { if (document.visibilityState === "visible") refresh(); };
    const settings = (event: Event) => {
      const changedDir = (event as CustomEvent<{ dir?: string }>).detail?.dir;
      if (changedDir === undefined || changedDir === dir) refresh();
    };
    window.addEventListener("focus", refresh);
    document.addEventListener("visibilitychange", visible);
    window.addEventListener(SETTINGS_CHANGED, settings);
    return () => {
      window.removeEventListener("focus", refresh);
      document.removeEventListener("visibilitychange", visible);
      window.removeEventListener(SETTINGS_CHANGED, settings);
    };
  }, [dir, refreshSelected]);

  const terminalResult = lastRun?.result;
  useLayoutEffect(() => {
    const token = selection.current;
    if (!terminalResult || !token || token.id !== terminalResult.id || detail.doc?.id !== terminalResult.id) return;
    const rows = new Map(terminalResult.outputs.map((row) => [row.key, row]));
    token.statusRead += 1;
    setDetail((old) => old.doc?.id === terminalResult.id
      ? { ...old, reading: false, doc: { ...old.doc, outputs: old.doc.outputs.map((row) => ({ ...row, ...rows.get(row.key) })) } }
      : old);
  }, [terminalResult]);

  useEffect(() => {
    const token = selection.current;
    if (!selectedRun?.active || !token || token.id !== selectedRun.id) return;
    const active = eventRefresh.current;
    if (active?.token === token && active.epoch === token.epoch) {
      active.queued = true;
      return;
    }
    const request = { token, epoch: token.epoch, queued: false };
    eventRefresh.current = request;
    void (async () => {
      do {
        request.queued = false;
        await refreshSelected();
      } while (request.queued && selection.current === token && token.epoch === request.epoch && activeRuns.has(dir));
      if (eventRefresh.current === request) eventRefresh.current = undefined;
    })();
  }, [dir, selectedRun?.id, selectedRun?.active, selectedRun?.version, refreshSelected]);

  const select = (token: Selection, id: string, doc: BatchDocument | null = null) => {
    token.id = id;
    token.epoch += 1;
    token.statusRead += 1;
    setBatchId(id);
    setDetail({ doc, reading: !!id });
    setCoverAt({});
    setCoverError(undefined);
    setActionError(undefined);
  };

  const choose = (id: string) => {
    const token = selection.current;
    if (!token) return;
    select(token, id);
    void refreshSelected();
  };

  const timelineOptions = ["", ...variants];
  const planned = timelineOptions.filter((variant) => timelines.has(variant)).flatMap((variant) =>
    presetOrder.filter((preset) => presets.has(preset)).map((preset) => ({ variant: variant || undefined, preset })),
  );
  const canCreate = planned.length > 0 && !writing && !running;

  const write = async <T,>(kind: "create" | "cover", operation: () => Promise<T>, apply: (result: T, token: Selection) => void) => {
    const token = selection.current;
    if (!token || token.dir !== dir || currentProject(dir).write || activeRuns.has(dir)) return;
    const epoch = token.epoch;
    const stillSelected = () => selection.current === token && token.epoch === epoch;
    setActionError(undefined);
    publish(dir, { write: kind });
    try {
      const result = await operation();
      if (stillSelected()) apply(result, token);
      try { await onChanged?.(dir); }
      catch (error) { if (selection.current === token && (kind === "create" || stillSelected())) setActionError(errorMessage(error)); }
      return result;
    } catch (error) {
      if (stillSelected()) setActionError(errorMessage(error));
    } finally {
      publish(dir, { write: undefined, revision: currentProject(dir).revision + 1 });
    }
  };

  const create = async () => {
    if (!canCreate) return;
    const created = await write("create", () => batchCreate(dir, planned), (document, token) => select(token, document.id, document));
    return created?.id;
  };

  const { doc, reading, drift, compareError, readError } = detail;
  const outputs = doc?.outputs ?? [];
  const qualified = outputs.filter((row) => row.state === "qualified").length;
  const left = outputs.length - qualified;
  const fullyDelivered = outputs.length > 0 && left === 0;
  const available = !!doc && !running && !writing && !reading && !compareError && !readError;
  const canRun = available && left > 0 && !drift;
  const canRerender = available && outputs.length > 0 && !!drift;
  const run = async (finishing?: "current") => {
    if (!selection.current || !doc || currentProject(dir).write || activeRuns.has(dir) || (finishing ? !canRerender : !canRun)) return;
    const id = doc.id;
    const job = "batch-ui-" + Date.now().toString(36) + "-" + Math.random().toString(36).slice(2);
    const tag = "batch-" + job;
    const record: ActiveRun = { job, id, active: true, version: 0, registered: false, sendingCancel: false };
    activeRuns.set(dir, record);
    setActionError(undefined);
    setCoverError(undefined);
    const notify = () => {
      const { cancel, registered, sendingCancel, ...view } = record;
      publish(dir, { run: { ...view, cancelSending: sendingCancel } });
    };
    let unsubscribe: (() => void) | undefined;
    let retryOnRegistration = false;
    const requestCancel = async () => {
      if (!record.active || record.sendingCancel || record.cancelPending) return;
      const registered = record.registered;
      retryOnRegistration = false;
      record.sendingCancel = true;
      record.cancelDeclined = false;
      record.cancelError = undefined;
      notify();
      try {
        const reply = await recipeCancel(job);
        if (!record.active) return;
        if (reply.cancelled) record.cancelPending = true;
        else {
          record.cancelDeclined = true;
          retryOnRegistration = !registered;
        }
      } catch (error) {
        if (record.active) record.cancelError = errorMessage(error);
      } finally {
        record.sendingCancel = false;
        if (record.active) {
          notify();
          if (retryOnRegistration && record.registered) {
            retryOnRegistration = false;
            void requestCancel();
          }
        }
      }
    };
    record.cancel = () => { void requestCancel(); };
    const unlisten = (off: () => void) => {
      try { off(); } catch { /* The native result is independent of the progress subscription. */ }
    };
    notify();

    // Native work, including cancellation, must remain usable when progress setup is slow or unavailable.
    try {
      void onEngineEvent(({ tag: frameTag, event }) => {
        if (!record.active || frameTag !== tag || event.event !== "batch.output" || event.batch !== id) return;
        if (!record.registered) {
          record.registered = true;
          if (retryOnRegistration && !record.sendingCancel) {
            retryOnRegistration = false;
            void requestCancel();
          }
        }
        record.version += 1;
        notify();
      }).then((off) => {
        if (record.active) unsubscribe = off;
        else unlisten(off);
      }).catch(() => undefined);
    } catch { /* The request proceeds without live progress. */ }

    try {
      record.result = await batchRun(dir, id, job, tag, finishing);
    } catch (error) {
      const code = errorCode(error);
      record.cancelled = code === "cancelled" || code === "render_cancelled";
      if (!record.cancelled) record.error = errorMessage(error);
    } finally {
      if (unsubscribe) unlisten(unsubscribe);
      record.active = false;
      record.cancelPending = false;
      record.cancelDeclined = false;
      record.cancelError = undefined;
      activeRuns.delete(dir);
      const { cancel, registered, sendingCancel, ...view } = record;
      publish(dir, { run: view, revision: currentProject(dir).revision + 1 });
      try { await onChanged?.(dir); }
      catch (error) {
        if (currentProject(dir).run?.job === job && !view.error && !view.cancelled) {
          publish(dir, { run: { ...view, error: errorMessage(error) } });
        }
      }
    }
  };

  const coverTime = (key: string) => coverAt[key] ?? "0";
  const canEditCover = (row: BatchOutputRow) => Boolean(row.output);
  const canSetCover = (row: BatchOutputRow) => canEditCover(row) && !writing && !running;
  const setCover = async (row: BatchOutputRow) => {
    if (!doc || !canSetCover(row)) return;
    const value = coverTime(row.key).trim();
    const seconds = Number(value);
    if (!value || !Number.isFinite(seconds) || seconds < 0) {
      setCoverError(row.key);
      return;
    }
    setCoverError(undefined);
    await write("cover", () => batchCover(dir, doc.id, row.key, Math.round(seconds * 1000)), (result, token) => {
      token.statusRead += 1;
      setDetail((old) => old.doc?.id === doc.id
        ? { ...old, reading: false, doc: { ...old.doc, outputs: old.doc.outputs.map((item) => item.key === row.key ? { ...item, ...result } : item) } }
        : old);
    });
  };

  return {
    coverUrl: (cover: BatchCover) => coverUrl(dir, cover),
    presets, setPreset: (preset: BatchPreset, checked: boolean) => setPresets((old) => toggle(old, preset, checked)),
    timelines, setTimeline: (value: string, checked: boolean) => setTimelines((old) => toggle(old, value, checked)),
    timelineOptions, planned, batches, batchId, doc, outputs, coverError,
    coverTime, setCoverTime: (key: string, value: string) => setCoverAt((old) => ({ ...old, [key]: value })),
    canCreate, canEditCover, canSetCover,
    creating: writing === "create", running, runningBatchId: running ? lastRun?.id : undefined,
    result: selectedRun?.result, cancelled: selectedRun?.cancelled ?? false,
    runError: selectedRun?.error, error: actionError ?? listError, compareError, readError, drift,
    qualified, left, fullyDelivered, canRun, canRerender, choose, create, run, setCover,
    cancelling: lastRun?.cancelPending === true || lastRun?.cancelSending === true,
    cancelDeclined: lastRun?.cancelDeclined === true,
    cancelError: lastRun?.cancelError,
    cancel: () => activeRuns.get(dir)?.cancel?.(),
  };
}
