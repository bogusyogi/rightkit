import { useDismiss } from "@rightkit/platform-ui/headless";
import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { engineCall, errorMessage, revealPath } from "../../lib/engine.js";
import { captionsShow, settingsSet, SETTINGS_CHANGED, type CaptionCue } from "../../lib/media.js";
import { useMediaPlaybackId } from "../../lib/mediaAdapters.js";
import { pickFile } from "../lib/dialog.js";
import { genRightCallFailed, interpretAssetLibrary, interpretHost, interpretJobs, interpretRecover, placeModesFor, type GeneratedAsset, type HostInfo, type HostJob, type PlaceMode } from "../lib/genright.js";
import { durationControl, ofKind, parseCatalog, pickModel, type CatalogKind, type ModelRow } from "../lib/models.js";

export type VideoAssetFilter = "all" | "broll" | "image" | "voice" | "music" | "sfx";
export function videoAssetMatches(kind: string | null, filter: VideoAssetFilter): boolean {
  if (filter === "all") return true;
  const value = (kind ?? "").toLowerCase();
  return ({ broll: ["broll", "video"], image: ["image", "still"], voice: ["voice", "narration"], music: ["music", "song"], sfx: ["sfx", "sound"] }[filter]).includes(value);
}
export const videoAssetPath = (dir: string, asset: GeneratedAsset) => /^(?:[a-z]:[\\/]|[\\/]{1,2})/i.test(asset.path) ? asset.path : `${dir}/${asset.path}`;
export const videoAssetIsAudio = (asset: GeneratedAsset) => /^audio\//.test(asset.mime) || /\.(wav|mp3|m4a|flac|aac|ogg)$/i.test(asset.path);
export const videoAssetIsImage = (asset: GeneratedAsset) => /^image\//.test(asset.mime) || /\.(png|jpe?g|webp)$/i.test(asset.path);
export function videoAssetJobAction(job: HostJob): "cancel" | "import" | "retry" | null {
  if (job.state === "queued" || job.state === "running") return "cancel";
  if (job.state === "ready") return "import";
  if (job.state === "failed") return "retry";
  return null;
}

const libraryListeners = new Map<string, Set<() => void>>();
const recoveries = new Map<string, Promise<ReturnType<typeof interpretRecover>>>();
const placements = new Set<string>();

function refreshAssetLibrary(dir: string) {
  for (const refresh of libraryListeners.get(dir) ?? []) refresh();
}

function recoverInterruptedJobs(dir: string, onChanged: (dir: string) => void | Promise<void>) {
  const pending = recoveries.get(dir);
  if (pending) return pending;
  const request = engineCall("genright.recover", { dir }).then(interpretRecover)
    .finally(() => {
      recoveries.delete(dir);
      refreshAssetLibrary(dir);
      // Registration can succeed before a placement or a later request fails.
      void Promise.resolve().then(() => onChanged(dir)).catch(() => undefined);
    });
  recoveries.set(dir, request);
  return request;
}

interface AssetRead {
  pending: boolean;
  queued: boolean;
}

async function readAssetState(read: AssetRead, queue: boolean, current: () => boolean, run: () => Promise<void>) {
  if (read.pending) {
    if (queue) read.queued = true;
    return;
  }
  read.pending = true;
  try {
    do {
      read.queued = false;
      await run();
    } while (read.queued && current());
  } finally { read.pending = false; }
}

interface AssetLibraryScope {
  dir: string | undefined;
  ready: boolean;
  statusRequest: number;
  jobsRead: AssetRead;
  assetsRead: AssetRead;
  changing: boolean;
  locating: boolean;
  recovering: boolean;
}

/** Library state; `onChanged` refreshes the captured project after a committed write. */
export function useVideoAssets({ dir, onChanged, onRecovered, onActionError, onLocated, captureSelection }: {
  dir?: string;
  onChanged: (dir: string) => void | Promise<void>;
  onRecovered?: (count: number) => void;
  onActionError?: (error: string) => void;
  onLocated?: () => void;
  captureSelection?: () => () => boolean;
}) {
  const [host, setHost] = useState<HostInfo>({ state: "checking" });
  const [jobs, setJobs] = useState<HostJob[] | null>(null);
  const [assets, setAssets] = useState<GeneratedAsset[] | null>(null);
  const [preview, setPreview] = useState<GeneratedAsset | null>(null);
  const previewPath = dir && preview ? videoAssetPath(dir, preview) : null;
  const previewPlaybackId = useMediaPlaybackId(preview && !videoAssetIsImage(preview) ? previewPath : null);
  const [filter, setFilter] = useState<VideoAssetFilter>("all");
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [recovered, setRecovered] = useState<Set<string>>(new Set());
  const scope = useRef<AssetLibraryScope | null>(null);
  const isActive = useCallback(() => scope.current !== null && scope.current.dir === dir, [dir]);
  const reportActionError = (e: unknown, token: AssetLibraryScope) => {
    if (onActionError) onActionError(errorMessage(e));
    else if (scope.current === token) setError(errorMessage(e));
  };
  useLayoutEffect(() => {
    const token: AssetLibraryScope = {
      dir, ready: false, statusRequest: 0, changing: false, locating: false, recovering: false,
      jobsRead: { pending: false, queued: false }, assetsRead: { pending: false, queued: false },
    };
    scope.current = token;
    setHost({ state: "checking" });
    setJobs(null); setAssets(null); setPreview(null); setError(null); setRecovered(new Set()); setBusy(null);
    return () => { if (scope.current === token) scope.current = null; };
  }, [dir]);
  const refresh = useCallback(async (queue = true) => {
    const token = scope.current;
    if (!dir || !token || token.dir !== dir) return;
    const current = () => scope.current === token;
    const failed = (e: unknown) => { if (current()) setError(errorMessage(e)); };
    await Promise.all([
      readAssetState(token.jobsRead, queue, current, () => engineCall("genright.jobs", { dir, live: token.ready })
        .then((value) => { if (current()) setJobs(interpretJobs(value)); }).catch(failed)),
      readAssetState(token.assetsRead, queue, current, () => engineCall("assets.list", { dir })
        .then((value) => { if (current()) setAssets(interpretAssetLibrary(value).assets); }).catch(failed)),
    ]);
  }, [dir]);
  const checkStatus = useCallback(async () => {
    const token = scope.current;
    if (!token || token.dir !== dir) return;
    const request = ++token.statusRequest;
    let status: HostInfo;
    try { status = interpretHost(await engineCall("genright.status", {})); }
    catch (e) { status = genRightCallFailed(errorMessage(e)); }
    if (scope.current !== token || token.statusRequest !== request) return;
    const ready = status.state === "ready";
    const liveChanged = token.ready !== ready;
    token.ready = ready;
    setHost(status);
    if (liveChanged) void refresh();
  }, [dir, refresh]);
  useEffect(() => {
    const settingsChanged = (event: Event) => {
      if (!(event as CustomEvent<{ dir?: string }>).detail?.dir) void checkStatus();
    };
    void checkStatus();
    window.addEventListener(SETTINGS_CHANGED, settingsChanged);
    return () => window.removeEventListener(SETTINGS_CHANGED, settingsChanged);
  }, [checkStatus]);
  useEffect(() => {
    if (!dir) return;
    const listener = () => { void refresh(); };
    const listeners = libraryListeners.get(dir) ?? new Set<() => void>();
    listeners.add(listener);
    libraryListeners.set(dir, listeners);
    return () => {
      listeners.delete(listener);
      if (listeners.size === 0) libraryListeners.delete(dir);
    };
  }, [dir, refresh]);
  useEffect(() => { void refresh(); }, [refresh]);
  const jobsActive = (jobs ?? []).some((job) => job.state === "queued" || job.state === "running");
  useEffect(() => {
    if (!jobsActive) return;
    const timer = window.setInterval(() => void refresh(false), 3000);
    return () => window.clearInterval(timer);
  }, [jobsActive, refresh]);
  useEffect(() => {
    const token = scope.current;
    if (!dir || !token || token.dir !== dir || !token.ready || token.recovering) return;
    token.recovering = true;
    void recoverInterruptedJobs(dir, onChanged).then((done) => {
      if (scope.current !== token) return;
      setRecovered(new Set(done.map((row) => row.requestId)));
      const count = done.filter((row) => row.state === "done").length;
      if (count) onRecovered?.(count);
    }).catch(() => undefined);
  }, [dir, host.state, onChanged, onRecovered]);
  const locate = async (title: string) => {
    const token = scope.current;
    if (!token || token.locating) return false;
    const stillSelected = captureSelection?.() ?? (() => scope.current === token);
    token.locating = true;
    try {
      const path = await pickFile(title, []);
      if (!path || !stillSelected()) return false;
      await settingsSet({ genright_engine_path: path });
      if (stillSelected()) onLocated?.();
      return true;
    } catch (e) { if (stillSelected()) reportActionError(e, token); return false; }
    finally { token.locating = false; }
  };
  const mutate = async (key: string, command: string, args: Record<string, unknown>) => {
    const token = scope.current;
    if (!dir || !token || token.dir !== dir || token.changing) return false;
    token.changing = true; setBusy(key); setError(null);
    let succeeded = false;
    try {
      await engineCall(command, { dir, ...args });
      succeeded = true;
      return true;
    } catch (e) { if (scope.current === token) setError(errorMessage(e)); return false; }
    finally {
      token.changing = false;
      if (scope.current === token) setBusy(null);
      refreshAssetLibrary(dir);
      void Promise.resolve().then(() => onChanged(dir)).catch((e) => {
        if (succeeded && scope.current === token) setError(errorMessage(e));
      });
    }
  };
  const jobAction = (job: HostJob) => {
    const action = videoAssetJobAction(job);
    if (!action) return Promise.resolve(false);
    return mutate(job.requestId, action === "cancel" ? "genright.cancel" : "genright.recover", { request_id: job.requestId, ...(action === "cancel" ? {} : { retry_failed: action === "retry" }) });
  };
  const relink = (asset: GeneratedAsset) => mutate(asset.assetId, "assets.relink", { asset_id: asset.assetId });
  const reveal = async (asset: GeneratedAsset) => {
    const token = scope.current;
    if (!dir || !token) return;
    const stillSelected = captureSelection?.() ?? (() => scope.current === token);
    try { await revealPath(videoAssetPath(dir, asset)); }
    catch (e) { if (stillSelected()) reportActionError(e, token); }
  };
  return { host, jobs, assets, preview, setPreview, previewPath, previewPlaybackId, filter, setFilter, shown: (assets ?? []).filter((asset) => videoAssetMatches(asset.kind, filter)), busy, error, recovered, jobsActive, checkStatus, refresh, locate, jobAction, relink, reveal, isActive };
}

export function useVideoAssetPlacement({ dir, asset, onChanged, onPlaced, onError }: {
  dir: string;
  asset: GeneratedAsset;
  onChanged: (dir: string) => void | Promise<void>;
  onPlaced?: () => void;
  onError?: (error: string) => void;
}) {
  const [open, setOpen] = useState(false);
  const container = useRef<HTMLDivElement | null>(null);
  const [mode, setMode] = useState<PlaceMode | null>(null);
  const [start, setStart] = useState(""); const [link, setLink] = useState(""); const [gain, setGain] = useState("");
  const [cues, setCues] = useState<CaptionCue[] | null>(null);
  const [cueSelection, setCueSelection] = useState<{ a: number; b: number } | null>(null);
  const [busy, setBusy] = useState(false); const [error, setError] = useState<string | null>(null);
  const scope = useRef<{ pending: boolean } | null>(null);
  const feedback = useRef(onError);
  feedback.current = onError;
  useLayoutEffect(() => {
    const token = { pending: false };
    scope.current = token;
    setOpen(false); setMode(null); setCues(null); setCueSelection(null); setStart(""); setLink(""); setGain(""); setError(null); setBusy(false);
    return () => { if (scope.current === token) scope.current = null; };
  }, [dir, asset.assetId]);
  const close = () => { setOpen(false); setMode(null); };
  useDismiss(close, { enabled: open, container, escape: false });
  useEffect(() => {
    if (mode !== "broll") return;
    const token = scope.current; let live = true;
    setCues(null); setCueSelection(null);
    void captionsShow(dir).then((result) => { if (live && scope.current === token) setCues(result.cues); }).catch((e) => {
      if (!live || scope.current !== token) return;
      const message = errorMessage(e);
      setCues([]); setError(message); feedback.current?.(message);
    });
    return () => { live = false; };
  }, [dir, mode]);
  const selectCue = (index: number, extend: boolean) => setCueSelection((old) => extend && old ? { a: old.a, b: index } : { a: index, b: index });
  const low = cueSelection && Math.min(cueSelection.a, cueSelection.b); const high = cueSelection && Math.max(cueSelection.a, cueSelection.b);
  const range = cues && low !== null && high !== null && cues[low] && cues[high] ? { start: cues[low].start_ms / 1000, end: cues[high].end_ms / 1000 } : null;
  const number = (value: string) => value.trim() !== "" && Number.isFinite(Number(value)) ? Number(value) : undefined;
  const needsRange = mode === "broll";
  const needsLink = mode === "narration";
  const needsGain = mode === "music";
  const canPlace = !!mode && !busy && (!needsRange || !!range);
  const isCueSelected = (index: number) => low !== null && high !== null && index >= low && index <= high;
  const place = async (selected: PlaceMode) => {
    const token = scope.current;
    const key = JSON.stringify([dir, asset.assetId]);
    if (!token || placements.has(key) || !placeModesFor(asset.kind).includes(selected) || (selected === "broll" && !range)) return false;
    placements.add(key);
    token.pending = true; setBusy(true); setError(null);
    const from = selected === "broll" ? range?.start : number(start);
    const to = selected === "broll" ? range?.end : undefined;
    const gainDb = number(gain);
    let succeeded = false;
    const report = (e: unknown) => {
      const message = errorMessage(e);
      if (scope.current === token) setError(message);
      onError?.(message);
    };
    try {
      await engineCall("assets.place", {
        dir, asset_id: asset.assetId, mode: selected,
        ...(from !== undefined ? { start_s: from } : {}),
        ...(to !== undefined ? { end_s: to } : {}),
        ...(link.trim() ? { link_clip: link.trim() } : {}),
        ...(gainDb !== undefined ? { gain_db: gainDb } : {}),
      });
      succeeded = true;
      if (scope.current === token) close();
      onPlaced?.();
      return true;
    } catch (e) { report(e); return false; }
    finally {
      placements.delete(key);
      token.pending = false;
      if (scope.current === token) setBusy(false);
      refreshAssetLibrary(dir);
      void Promise.resolve().then(() => onChanged(dir)).catch((e) => {
        if (succeeded) report(e);
      });
    }
  };
  const modes = placeModesFor(asset.kind);
  const chooseMode = (selected: PlaceMode) => {
    if (!modes.includes(selected) || scope.current?.pending) return Promise.resolve(false);
    if (selected === "append") return place(selected);
    setMode(selected);
    return Promise.resolve(false);
  };
  const toggle = () => { if (!scope.current?.pending) setOpen((wasOpen) => !wasOpen); };
  const back = () => setMode(null);
  const placeSelected = () => mode ? place(mode) : Promise.resolve(false);
  return { open, container, toggle, modes, mode, chooseMode, back, start, setStart, link, setLink, gain, setGain, cues,
    selectCue, isCueSelected, range, needsRange, needsLink, needsGain, canPlace, busy, error, placeSelected };
}

export function useVideoAssetGeneration({ dir, open, onChanged, onSucceeded, onError }: {
  dir: string;
  open: boolean;
  onChanged: (dir: string) => void | Promise<void>;
  onSucceeded?: () => void;
  onError?: (error: string) => void;
}) {
  const [kind, setKind] = useState<CatalogKind>("video"); const [prompt, setPrompt] = useState("");
  const [models, setModels] = useState<ModelRow[] | null>(null); const [model, setModelState] = useState<ModelRow | null>(null);
  const [duration, setDuration] = useState<number | undefined>(); const [busy, setBusy] = useState(false); const [error, setError] = useState<string | null>(null);
  const scope = useRef<{ pending: boolean } | null>(null);
  const openCycle = useRef<object | null>(null);
  useLayoutEffect(() => { const cycle = open ? {} : null; openCycle.current = cycle; return () => { if (openCycle.current === cycle) openCycle.current = null; }; }, [dir, open]);
  useLayoutEffect(() => {
    const token = { pending: false };
    scope.current = token;
    setKind("video"); setPrompt(""); setModels(null); setModelState(null); setDuration(undefined); setError(null); setBusy(false);
    return () => { if (scope.current === token) scope.current = null; };
  }, [dir]);
  useEffect(() => {
    if (!open) return;
    const token = scope.current; let live = true; setModels(null);
    void engineCall("genright.models", {}).then((result) => { if (live && scope.current === token) setModels(parseCatalog(result).models); }).catch(() => { if (live && scope.current === token) setModels([]); });
    return () => { live = false; };
  }, [dir, open]);
  useEffect(() => { const next = models ? pickModel({ models, structuredDurations: true }, kind) : undefined; setModelState(next ?? null); setDuration(next ? durationControl(next).initial : undefined); }, [kind, models]);
  const setModel = (value: ModelRow | null) => { setModelState(value); setDuration(value ? durationControl(value).initial : undefined); };
  const shownModels = models ? ofKind({ models, structuredDurations: true }, kind) : [];
  const chooseModel = (key: string) => setModel(shownModels.find((row) => `${row.provider}/${row.model}` === key) ?? null);
  const needsSource = kind === "upscale" || kind === "lipsync";
  const validDuration = duration === undefined || (Number.isFinite(duration) && duration > 0 && duration <= 600);
  const canSubmit = open && !!model && shownModels.includes(model) && !!prompt.trim() && !needsSource && validDuration && !busy;
  const submit = async () => {
    const token = scope.current;
    const cycle = openCycle.current;
    if (!canSubmit || !model || !token || token.pending || !cycle) return false;
    token.pending = true; setBusy(true); setError(null);
    const requestId = `ui-gen-${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}`;
    let succeeded = false;
    const report = (e: unknown) => {
      const message = errorMessage(e);
      if (scope.current === token) setError(message);
      onError?.(message);
    };
    try {
      await engineCall("generate", { dir, kind, input: { provider: model.provider, model: model.model, prompt: prompt.trim(), request_id: requestId, ...(duration !== undefined ? { duration_s: duration } : {}) } });
      succeeded = true;
      onSucceeded?.();
      if (scope.current !== token || openCycle.current !== cycle) return false;
      setPrompt("");
      return true;
    } catch (e) {
      report(e);
      return false;
    }
    finally {
      token.pending = false;
      if (scope.current === token) setBusy(false);
      refreshAssetLibrary(dir);
      void Promise.resolve().then(() => onChanged(dir)).catch((e) => {
        if (succeeded) report(e);
      });
    }
  };
  return { kind, setKind, prompt, setPrompt, models, model, setModel, chooseModel, shownModels, duration, setDuration, durationControl: durationControl(model ?? undefined), needsSource, busy, error, canSubmit, submit };
}

