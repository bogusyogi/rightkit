import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState, useSyncExternalStore } from "react";
import { videoAppHost } from "../../host.js";
import { assetUrl } from "../../lib/asset.js";
import { engineCall, errorMessage, onEngineEvent, revealPath } from "../../lib/engine.js";
import { mediaKind, useMediaPlaybackId } from "../../lib/mediaAdapters.js";
import { modelsStatus, settingsGet, settingsSet, sourceList, sourceProbe, sourceRelink, sourceVerify, thumbnails, transcribe, transcriptShow, type SourceProbe } from "../../lib/media.js";
import { projectShow, TICKS_PER_SECOND, type ProjectShow } from "../../lib/project.js";
import { pickFile, pickMedia } from "../lib/dialog.js";
import { micAlign, type MicAlignResult } from "../lib/mic.js";
import { useVideoRecipeTranscription } from "./useVideoRecipe.js";

export type VideoSourceRow = { id: string; name: string; mode?: string; path: string; size?: number; exists: boolean; has_transcript: boolean };
export type VideoSourceIntent = "talking-head" | "mixed" | "visual";
export type VideoMicState = { status: "importing" } | { status: "aligning" } | { status: "done"; result: MicAlignResult } | { status: "failed"; message: string };
type Scope = { dir: string; reads: number; requested: Map<string, { path: string }>; relinked: Set<string>; intents: Record<string, VideoSourceIntent>; intentEdits: number };
type SourceWork = {
  importing: number;
  addProgress: Record<string, { phase: string; fraction?: number }>;
  directTxStatus: Record<string, { message?: string; fraction?: number }>;
  micState: Record<string, VideoMicState>;
  actionError: string | null;
  relinkRejected: "mismatch" | "unverified" | null;
};
const idleWork: SourceWork = { importing: 0, addProgress: {}, directTxStatus: {}, micState: {}, actionError: null, relinkRejected: null };
const projectWork = new Map<string, SourceWork>();
const workListeners = new Map<string, Set<() => void>>();
const projectActions = new Map<string, Set<string>>();
const workTags = new Map<string, { dir: string; key: string }>();
const projectIntentWrites = new Map<string, Promise<void>>();
type SourceChange = { relinked?: string; reverify?: boolean };
const sourceListeners = new Map<string, Set<(change: SourceChange) => void>>();
const intentsOrder: VideoSourceIntent[] = ["talking-head", "mixed", "visual"];
const mediaPattern = /\.(mp4|mov|mkv|webm|m4v|wav|mp3|m4a|flac|aac|ogg)$/i;
const micExtensions = ["wav", "mp3", "m4a", "flac", "aac", "ogg", "mp4", "mov"];
const uniqueTag = (prefix: string) => `${prefix}-${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}`;
const stopCardAction = (event: { stopPropagation(): void }) => event.stopPropagation();

function updateWork(dir: string, update: (previous: SourceWork) => SourceWork) {
  projectWork.set(dir, update(projectWork.get(dir) ?? idleWork));
  for (const listener of workListeners.get(dir) ?? []) listener();
}

function useSourceWork(dir: string) {
  const subscribe = useCallback((listener: () => void) => {
    const listeners = workListeners.get(dir) ?? new Set<() => void>();
    listeners.add(listener);
    workListeners.set(dir, listeners);
    return () => { listeners.delete(listener); if (!listeners.size) workListeners.delete(dir); };
  }, [dir]);
  const snapshot = useCallback(() => projectWork.get(dir) ?? idleWork, [dir]);
  return useSyncExternalStore(subscribe, snapshot, snapshot);
}

export function useVideoSources({ dir, onChanged }: { dir: string; onChanged: (dir: string) => Promise<void> }) {
  const [rows, setRows] = useState<VideoSourceRow[]>([]);
  const [show, setShow] = useState<ProjectShow | null>(null);
  const [probes, setProbes] = useState<Record<string, SourceProbe>>({});
  const [checks, setChecks] = useState<Record<string, "ok" | "missing" | "changed">>({});
  const [verifiedCount, setVerifiedCount] = useState<number | null>(null);
  const [verifyRun, setVerifyRun] = useState(0);
  const { importing, addProgress, directTxStatus, micState, actionError, relinkRejected } = useSourceWork(dir);
  const recipeTx = useVideoRecipeTranscription(dir);
  const [speechReady, setSpeechReady] = useState<boolean | null>(null);
  const [intents, setIntents] = useState<Record<string, VideoSourceIntent>>({});
  const [error, setError] = useState<string | null>(null);
  const [dropActive, setDropActive] = useState(false);
  const [open, setOpen] = useState<string | null>(null);
  const [thumbSrc, setThumbSrc] = useState<Record<string, string>>({});
  const [excerpt, setExcerpt] = useState<string | null>(null);
  const [copied, setCopied] = useState<object | null>(null);
  const scope = useRef<Scope | null>(null);
  const drawerSelection = useRef(0);
  const openRow = rows.find((row) => row.id === open) ?? null;
  const playbackId = useMediaPlaybackId(openRow?.exists ? openRow.path : null);
  useLayoutEffect(() => {
    const token: Scope = { dir, reads: 0, requested: new Map(), relinked: new Set(), intents: {}, intentEdits: 0 };
    scope.current = token;
    setRows([]); setShow(null); setProbes({}); setChecks({}); setVerifiedCount(null); setSpeechReady(null); setIntents({}); setError(null); setDropActive(false); setOpen(null); setThumbSrc({}); setExcerpt(null); setCopied(null);
    return () => { if (scope.current === token) scope.current = null; };
  }, [dir]);
  const refresh = useCallback(async () => {
    const token = scope.current; if (!token || token.dir !== dir) return;
    const generation = ++token.reads;
    const intentsAtStart = token.intentEdits;
    await projectIntentWrites.get(dir)?.catch(() => undefined);
    const current = () => scope.current === token && token.reads === generation;
    if (!current()) return;
    try {
      const [list, project, settings, models] = await Promise.all([sourceList(dir), projectShow(dir).catch(() => null), settingsGet(dir).catch(() => null), modelsStatus().catch(() => null)]);
      if (!current()) return;
      setRows(list.sources as VideoSourceRow[]); setShow(project); setSpeechReady(models?.speech.ready ?? null);
      if (settings && token.intentEdits === intentsAtStart) {
        const saved = settings.project?.intents;
        token.intents = saved && typeof saved === "object" ? saved as Record<string, VideoSourceIntent> : {};
        setIntents(token.intents);
      }
    } catch (e) {
      if (current()) setError(errorMessage(e));
    }
  }, [dir]);
  useLayoutEffect(() => {
    const token = scope.current;
    if (!token) return;
    const listeners = sourceListeners.get(dir) ?? new Set<(change: SourceChange) => void>();
    const listener = ({ relinked, reverify }: SourceChange) => {
      if (scope.current !== token) return;
      if (relinked) {
        token.relinked.add(relinked);
        token.requested.delete(relinked);
        setChecks((old) => ({ ...old, [relinked]: "ok" }));
        setProbes(({ [relinked]: removed, ...rest }) => rest);
        setThumbSrc(({ [relinked]: removed, ...rest }) => rest);
      }
      if (reverify) setVerifyRun((run) => run + 1);
      void refresh();
    };
    listeners.add(listener);
    sourceListeners.set(dir, listeners);
    return () => { listeners.delete(listener); if (!listeners.size) sourceListeners.delete(dir); };
  }, [dir, refresh]);
  useEffect(() => { void refresh(); }, [refresh, recipeTx.readback]);
  useEffect(() => {
    const token = scope.current; if (!token) return;
    token.relinked.clear();
    const tag = uniqueTag("media-verify"); let live = true; let off: (() => void) | undefined;
    const unsubscribe = (remove: () => void) => { try { remove(); } catch { /* Verification is independent of progress. */ } };
    try {
      void onEngineEvent((frame) => {
        if (!live || scope.current !== token || frame.tag !== tag || frame.event.event !== "source.verify") return;
        if (typeof frame.event.completed === "number") setVerifiedCount(frame.event.completed);
      }).then((remove) => {
        if (live) off = remove;
        else unsubscribe(remove);
      }).catch(() => undefined);
    } catch { /* Verification can run without progress. */ }
    void (async () => {
      try {
        const result = await sourceVerify(dir, tag);
        if (!live || scope.current !== token) return;
        const map: Record<string, "ok" | "missing" | "changed"> = {};
        for (const check of result.checks ?? []) map[check.source_id] = check.status === "ok" ? "ok" : check.status === "missing" || check.status === "unreadable" ? "missing" : "changed";
        for (const id of token.relinked) map[id] = "ok";
        setChecks(map);
      } catch { /* Verification stays unknown on failure. */ }
      finally {
        live = false;
        if (off) unsubscribe(off);
        off = undefined;
        if (scope.current === token) setVerifiedCount(null);
      }
    })();
    return () => {
      live = false;
      if (off) unsubscribe(off);
      off = undefined;
    };
  }, [dir, verifyRun]);
  useEffect(() => {
    const token = scope.current; if (!token) return;
    let live = true; let off: (() => void) | undefined;
    const unsubscribe = (remove?: () => void) => { try { remove?.(); } catch { /* Native writes don't depend on progress. */ } };
    void Promise.resolve().then(() => onEngineEvent((frame) => {
      if (!live || scope.current !== token || !frame.tag) return;
      const target = workTags.get(frame.tag);
      if (target?.dir !== dir) return;
      const { key } = target;
      const event = frame.event;
      if (event.event === "source.add.progress") {
        const progress = { phase: typeof event.phase === "string" ? event.phase : "working", fraction: typeof event.fraction === "number" ? event.fraction : undefined };
        updateWork(dir, (old) => ({ ...old, addProgress: { ...old.addProgress, [key]: progress } }));
      }
      if (event.event === "transcribe.progress" && event.source === key) {
        const progress = { message: typeof event.message === "string" ? event.message : undefined, fraction: typeof event.fraction === "number" ? event.fraction : undefined };
        updateWork(dir, (old) => ({ ...old, directTxStatus: { ...old.directTxStatus, [key]: progress } }));
      }
    })).then((remove) => { if (live) off = remove; else unsubscribe(remove); }).catch(() => undefined);
    return () => { live = false; unsubscribe(off); };
  }, [dir]);
  useEffect(() => {
    const token = scope.current; if (!token) return;
    for (const row of rows) {
      if (!row.exists || token.requested.get(row.id)?.path === row.path) continue;
      const request = { path: row.path };
      token.requested.set(row.id, request);
      const current = () => scope.current === token && token.requested.get(row.id) === request;
      void thumbnails(dir, { source: row.id }, { width: 480 }).then((result) => { if (current() && result.frames[0]) setThumbSrc((old) => ({ ...old, [row.id]: assetUrl(result.frames[0].path) })); }).catch(() => undefined);
      void sourceProbe(dir, row.id).then((result) => { if (current()) setProbes((old) => ({ ...old, [row.id]: result })); }).catch(() => undefined);
    }
  }, [dir, rows]);
  const infoOf = useCallback((id: string) => show?.timeline.sources.find((source) => source.id === id), [show]);
  const intentOf = (row: VideoSourceRow): VideoSourceIntent => intents[row.id] ?? (infoOf(row.id)?.has_audio ? row.has_transcript ? "talking-head" : "mixed" : "visual");
  const txStatus = recipeTx.current ? { ...directTxStatus, [recipeTx.current.source]: recipeTx.current } : directTxStatus;
  const busyOf = (row: VideoSourceRow, index: number) => txStatus[row.id] ? "transcribing" : verifiedCount !== null && index >= verifiedCount && row.exists ? "hashing" : null;
  const withAction = useCallback(async (key: string, action: (token: Scope) => Promise<void>) => {
    const token = scope.current; if (!token || token.dir !== dir) return false;
    const active = projectActions.get(dir) ?? new Set<string>(); if (active.has(key)) return false;
    active.add(key); projectActions.set(dir, active);
    updateWork(dir, (old) => ({ ...old, actionError: null, relinkRejected: null }));
    try { await action(token); return true; }
    catch (e) { updateWork(dir, (old) => ({ ...old, actionError: errorMessage(e) })); return false; }
    finally { active.delete(key); if (!active.size) projectActions.delete(dir); }
  }, [dir]);
  const notifyChanged = useCallback(async (token: Scope, change: SourceChange = {}) => {
    for (const listener of sourceListeners.get(token.dir) ?? []) listener(change);
    try { await onChanged(token.dir); } catch (e) { if (scope.current === token) setError(errorMessage(e)); }
  }, [onChanged]);
  const importFiles = useCallback((files: string[]) => withAction("import", async (token) => {
    if (!files.length) return;
    updateWork(dir, (old) => ({ ...old, importing: old.importing + 1 }));
    setError(null);
    try {
      for (const file of files) {
        const tag = uniqueTag("ui-add");
        workTags.set(tag, { dir, key: file });
        updateWork(dir, (old) => ({ ...old, addProgress: { ...old.addProgress, [file]: { phase: "hashing" } } }));
        try { await engineCall("source.add", { dir, file }, tag); }
        finally {
          workTags.delete(tag);
          updateWork(dir, (old) => {
            const { [file]: removed, ...addProgress } = old.addProgress;
            return { ...old, addProgress };
          });
        }
      }
    } finally {
      await notifyChanged(token);
      updateWork(dir, (old) => ({ ...old, importing: old.importing - 1 }));
    }
  }), [dir, notifyChanged, withAction]);
  const importMedia = async () => { const token = scope.current; try { const files = await pickMedia(); if (scope.current === token) return await importFiles(files); } catch (e) { if (scope.current === token) setError(errorMessage(e)); } return false; };
  useEffect(() => {
    const token = scope.current;
    let live = true;
    let off: (() => void) | undefined;
    const unsubscribe = (remove?: () => void) => { try { remove?.(); } catch { /* Picker import is still available. */ } };
    void Promise.resolve().then(() => videoAppHost().onDragDrop((event) => {
      if (!live || scope.current !== token) return;
      if (event.type === "enter" || event.type === "over") setDropActive(true);
      else if (event.type === "leave") setDropActive(false);
      else if (event.type === "drop") {
        setDropActive(false);
        const files = event.paths.filter((file) => mediaPattern.test(file));
        if (files.length) void importFiles(files);
      }
    })).then((remove) => { if (live) off = remove; else unsubscribe(remove); }).catch(() => undefined);
    return () => { live = false; unsubscribe(off); };
  }, [dir, importFiles]);
  const relink = (row: VideoSourceRow) => withAction(`relink:${row.id}`, async (token) => {
    const picked = await pickMedia(); if (scope.current !== token || !picked[0]) return;
    let result: Awaited<ReturnType<typeof sourceRelink>>;
    try {
      result = await sourceRelink(dir, row.id, picked[0]);
    } catch (e) {
      await notifyChanged(token, { reverify: true });
      throw e;
    }
    const replacement = result.relinks?.find((record) => record.source_id === row.id);
    if (!replacement?.applied || !replacement.matches) {
      updateWork(dir, (old) => ({ ...old, relinkRejected: replacement?.reason === "content_mismatch" ? "mismatch" : "unverified" }));
      return;
    }
    await notifyChanged(token, { relinked: row.id });
  });
  const cycleIntent = (row: VideoSourceRow) => {
    const token = scope.current; if (!token || token.dir !== dir) return;
    const previous = token.intents[row.id] ?? intentOf(row);
    const next = intentsOrder[(intentsOrder.indexOf(previous) + 1) % intentsOrder.length];
    const updated = { ...token.intents, [row.id]: next }; token.intents = updated; setIntents(updated);
    token.intentEdits += 1;
    const pending = projectIntentWrites.get(dir) ?? Promise.resolve();
    const operation = pending.catch(() => undefined).then(async () => {
      const saved = (await settingsGet(dir)).project?.intents;
      const intents = saved && typeof saved === "object" ? saved : {};
      await settingsSet({ intents: { ...intents, [row.id]: next } }, dir);
    });
    projectIntentWrites.set(dir, operation);
    void operation.catch((error) => {
      if (scope.current !== token) return;
      setError(errorMessage(error));
      void refresh();
    }).finally(() => {
      if (projectIntentWrites.get(dir) === operation) projectIntentWrites.delete(dir);
    });
  };
  const alignMic = (row: VideoSourceRow, title: string) => withAction(`mic:${row.id}`, async (token) => {
    const file = await pickFile(title, micExtensions); if (scope.current !== token || !file) return;
    const setMic = (state: VideoMicState) => updateWork(dir, (old) => ({ ...old, micState: { ...old.micState, [row.id]: state } }));
    setMic({ status: "importing" });
    try {
      const result = await engineCall<{ source: { id: string } }>("source.add", { dir, file });
      if (scope.current === token) void refresh();
      setMic({ status: "aligning" });
      const alignment = await micAlign(dir, row.id, result.source.id, {}, uniqueTag("ui-mic"));
      setMic({ status: "done", result: alignment });
    } catch (e) { setMic({ status: "failed", message: errorMessage(e) }); }
    finally { await notifyChanged(token); }
  });
  const runTranscribe = (row: VideoSourceRow) => withAction(`transcribe:${row.id}`, async (token) => {
    if (!row.exists || speechReady === false || recipeTx.current?.source === row.id) return;
    const tag = uniqueTag("ui-tx");
    workTags.set(tag, { dir, key: row.id });
    updateWork(dir, (old) => ({ ...old, directTxStatus: { ...old.directTxStatus, [row.id]: {} } }));
    setError(null);
    try { await transcribe(dir, row.id, tag, true, tag); }
    finally {
      workTags.delete(tag);
      await notifyChanged(token);
      updateWork(dir, (old) => {
        const { [row.id]: removed, ...directTxStatus } = old.directTxStatus;
        return { ...old, directTxStatus };
      });
    }
  });
  useEffect(() => {
    const token = scope.current; let live = true; setExcerpt(null);
    if (openRow?.has_transcript) void transcriptShow(dir, openRow.id).then((result) => { if (live && scope.current === token) setExcerpt(result.transcript.words.slice(0, 60).map((word) => word.text).join(" ")); }).catch(() => { if (live && scope.current === token) setExcerpt(null); });
    return () => { live = false; };
  }, [dir, openRow]);
  const selectSource = (id: string | null) => {
    drawerSelection.current += 1;
    setCopied(null);
    setExcerpt(null);
    setOpen(id);
  };
  const copyHash = async () => {
    const token = scope.current;
    const selection = drawerSelection.current;
    if (!openRow || !navigator.clipboard) return;
    try {
      await navigator.clipboard.writeText(openRow.id);
      if (scope.current !== token || drawerSelection.current !== selection) return;
      const confirmation = {};
      setCopied(confirmation);
      window.setTimeout(() => { if (scope.current === token) setCopied((current) => current === confirmation ? null : current); }, 1500);
    } catch (e) {
      if (scope.current === token && drawerSelection.current === selection) setError(errorMessage(e));
    }
  };
  const totalMs = useMemo(() => rows.reduce((sum, row) => sum + ((infoOf(row.id)?.duration ?? 0) / TICKS_PER_SECOND) * 1000, 0), [rows, infoOf]);
  const describe = (row: VideoSourceRow) => {
    const info = infoOf(row.id);
    const probe = probes[row.id];
    const dimensions = probe ?? info?.video;
    return {
      durationMs: probe?.duration_ms ?? ((info?.duration ?? 0) / TICKS_PER_SECOND) * 1000,
      dimensions: dimensions ? { width: dimensions.width, height: dimensions.height } : null,
      fps: probe?.fps ? probe.fps.num / probe.fps.den : null,
      hasAudio: info?.has_audio,
      probe,
    };
  };
  const cards = rows.map((row, index) => {
    const missing = !row.exists || checks[row.id] === "missing";
    const changed = checks[row.id] === "changed";
    return {
      row,
      ...describe(row),
      missing,
      changed,
      busy: busyOf(row, index),
      intent: intentOf(row),
      transcriptStatus: txStatus[row.id],
      preview: missing || changed ? undefined : assetUrl(row.path),
      thumbnail: missing || changed ? undefined : thumbSrc[row.id],
    };
  });
  const drawer = openRow ? {
    row: openRow,
    ...describe(openRow),
    kind: mediaKind(openRow.path),
    playbackId,
    excerpt,
    copied: copied !== null,
    microphone: micState[openRow.id],
    canAlign: openRow.exists && micState[openRow.id]?.status !== "importing" && micState[openRow.id]?.status !== "aligning",
    canTranscribe: openRow.exists && speechReady !== false && !txStatus[openRow.id],
  } : null;
  return { cards, drawer, addProgress, importing, speechReady, error: actionError ?? error, relinkRejected, dropActive, totalMs, canMakeEdit: cards.some((card) => !card.missing && !card.changed), selectSource, stopCardAction, importMedia, relink, cycleIntent, alignMic, runTranscribe, copyHash, reveal: revealPath };
}
