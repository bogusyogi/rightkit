import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState, type PointerEvent as ReactPointerEvent } from "react";
import { assetUrl } from "../lib/asset.js";
import { engineCall, errorMessage } from "../lib/engine.js";
import { editplanList, thumbnails, type CaptionCue, type StoredPlan } from "../lib/media.js";
import type { ProjectShow } from "../lib/project.js";
import { EditSession, errorText } from "./adapter.js";
import type { CapabilityReport } from "./capabilities.js";
import { buildEditorModel } from "./model.js";
import { clipRanges } from "./transcript.js";

export type VideoTimelineTab = "transcript" | "changes" | "inspect" | "captions";
export type VideoTimelineMode = "review" | "edit";
export type VideoTimelinePreviewHost = { seek: (revisionCursor: number, timeS: number) => unknown };

function previewAspect(show: ProjectShow): string | null {
  const clip = show.timeline.video_tracks.flatMap((track) => track.clips)[0];
  if (!clip) return null;
  const video = show.sources.find(({ info }) => info.id === clip.source)?.info.video;
  if (!video?.width || !video.height) return null;
  const ratio = video.width / video.height;
  const common = [[16, 9], [9, 16], [1, 1], [4, 3]].find(([w, h]) => Math.abs(ratio - w / h) < 0.02);
  return common ? common.join(":") : `${video.width}×${video.height}`;
}

/** Revision-bound editor behavior; the host supplies presentation and tab labels. */
export function useVideoTimelineEditor({ show, previewHost, onRefresh, busy }: {
  show: ProjectShow;
  previewHost: VideoTimelinePreviewHost | null;
  onRefresh: () => Promise<unknown>;
  busy: boolean;
}) {
  const dir = show.project.dir;
  const revision = show.revision.cursor;
  const revisionOwner = useMemo(() => ({ dir, revision }), [dir, revision]);
  const liveRevision = useRef<typeof revisionOwner | null>(null);
  useLayoutEffect(() => {
    liveRevision.current = revisionOwner;
    return () => { if (liveRevision.current === revisionOwner) liveRevision.current = null; };
  }, [revisionOwner]);
  const caps: CapabilityReport = useMemo(() => ({ extensions: show.extensions }), [show.extensions]);
  const [tab, setTab] = useState<VideoTimelineTab>("transcript");
  const [mode, setMode] = useState<VideoTimelineMode>("review");
  const [captionsOn, setCaptionsOn] = useState(true);
  const [plans, setPlans] = useState<StoredPlan[]>([]);
  const [plansError, setPlansError] = useState<string | null>(null);
  const [panelError, setPanelError] = useState<string | null>(null);
  const [playheadS, setPlayheadS] = useState(0);
  const [selected, setSelected] = useState<ReadonlySet<string>>(new Set());
  const [fallback, setFallback] = useState<string | null>(null);
  const [timelineHeight, setTimelineHeight] = useState(192);
  const [timelineFull, setTimelineFull] = useState(false);
  const [sideHidden, setSideHidden] = useState(false);
  const [refreshing, setRefreshing] = useState(false);
  const [requestedSeek, setRequestedSeek] = useState<{ owner: typeof revisionOwner; ms: number; n: number } | null>(null);
  const seekState = requestedSeek?.owner === revisionOwner ? requestedSeek : null;
  const refreshBinding = useMemo(() => ({ run: onRefresh }), [dir]);
  const lifetime = useRef<{ binding: typeof refreshBinding; pendingRefreshes: number } | null>(null);
  const stopResize = useRef<(() => void) | null>(null);
  useLayoutEffect(() => { refreshBinding.run = onRefresh; }, [onRefresh, refreshBinding]);
  useLayoutEffect(() => {
    const token = { binding: refreshBinding, pendingRefreshes: 0 };
    lifetime.current = token;
    setTab("transcript"); setMode("review"); setPlans([]); setPlansError(null); setPanelError(null); setPlayheadS(0); setSelected(new Set()); setRequestedSeek(null); setRefreshing(false);
    return () => { if (lifetime.current === token) lifetime.current = null; stopResize.current?.(); };
  }, [refreshBinding]);
  const built = useMemo(() => {
    try { return { model: buildEditorModel(show), error: null }; }
    catch (e) { return { model: null, error: errorMessage(e) }; }
  }, [show]);
  const model = built.model;
  const empty = !model || model.lanes.every((lane) => lane.clips.length === 0);
  const aspect = useMemo(() => previewAspect(show), [show]);
  const cues: CaptionCue[] = useMemo(() => model ? model.captions.map((cue) => ({ start_ms: cue.start_frame * 1000 / model.fps, end_ms: (cue.start_frame + cue.frames) * 1000 / model.fps, text: cue.text, lines: [cue.text] })).sort((a, b) => a.start_ms - b.start_ms) : [], [model]);
  useEffect(() => {
    const token = lifetime.current; let cancelled = false;
    setPlansError(null);
    void (async () => {
      for (let attempt = 0; attempt < 3; attempt += 1) {
        try { const result = await editplanList(dir); if (!cancelled && lifetime.current === token) setPlans(result.plans); return; }
        catch (e) {
          if (cancelled || lifetime.current !== token) return;
          if (attempt === 2) { setPlans([]); setPlansError(errorText(e)); return; }
          await new Promise((resolve) => setTimeout(resolve, 600));
        }
      }
    })();
    return () => { cancelled = true; };
  }, [dir, revision]);
  const ranges = useMemo(() => clipRanges(show), [show]);
  const firstSource = model?.lanes.find((lane) => lane.kind === "video")?.clips[0]?.ref;
  const firstSourceId = firstSource?.kind === "video" ? firstSource.clip.source : null;
  useLayoutEffect(() => { setFallback(null); }, [dir, revision]);
  useEffect(() => {
    const token = lifetime.current; let live = true;
    if (!firstSourceId) return;
    const rows = ranges.get(firstSourceId) ?? [];
    const range = rows.find((row) => playheadS >= row.outOffS && playheadS < row.outOffS + (row.outS - row.inS)) ?? rows.reduce<(typeof rows)[number] | null>((best, row) => !best || Math.abs(row.outOffS - playheadS) < Math.abs(best.outOffS - playheadS) ? row : best, null);
    if (!range) return;
    const at = range.inS + Math.max(0, playheadS - range.outOffS);
    const timer = setTimeout(() => {
      void thumbnails(dir, { source: firstSourceId }, { at_ms: Math.round(at * 1000), width: 1280 }).then((result) => {
        if (live && lifetime.current === token && result.frames[0]?.path) setFallback(assetUrl(result.frames[0].path));
      }).catch(() => undefined);
    }, 200);
    return () => { live = false; clearTimeout(timer); };
  }, [dir, revision, firstSourceId, playheadS, ranges]);
  const refresh = useCallback(async () => {
    const token = lifetime.current;
    const local = token?.binding === refreshBinding ? token : null;
    if (local) {
      local.pendingRefreshes += 1;
      setRefreshing(true);
    }
    try { await refreshBinding.run(); }
    catch (e) { if (local && lifetime.current === local) setPanelError(errorText(e)); }
    finally {
      if (local) {
        local.pendingRefreshes -= 1;
        if (lifetime.current === local && local.pendingRefreshes === 0) setRefreshing(false);
      }
    }
  }, [refreshBinding]);
  const committed = useCallback(() => { void refresh(); }, [refresh]);
  const currentSession = useRef<EditSession | null>(null);
  const session = useMemo(() => {
    const value = new EditSession(dir, revision, caps, engineCall, () => {
      if (currentSession.current === value) void refresh();
    });
    return value;
  }, [dir]);
  useLayoutEffect(() => {
    currentSession.current = session;
    return () => { if (currentSession.current === session) currentSession.current = null; };
  }, [session]);
  useLayoutEffect(() => { session.resync(revision); }, [session, revision]);
  const onSeekPreview = useCallback((cursor: number, timeS: number) => {
    if (cursor !== revisionOwner.revision || liveRevision.current !== revisionOwner) return;
    setRequestedSeek((old) => ({ owner: revisionOwner, ms: Math.round(timeS * 1000), n: (old?.n ?? 0) + 1 }));
    previewHost?.seek(cursor, timeS);
  }, [previewHost, revisionOwner]);
  const seek = (timeS: number) => {
    if (liveRevision.current !== revisionOwner) return;
    setPlayheadS(timeS);
    onSeekPreview(revision, timeS);
  };
  const onPosition = useCallback((ms: number) => {
    if (liveRevision.current === revisionOwner) setPlayheadS(ms / 1000);
  }, [revisionOwner]);
  const currentCue = cues.find((cue) => playheadS * 1000 >= cue.start_ms && playheadS * 1000 < cue.end_ms);
  const dragTimeline = (event: ReactPointerEvent) => {
    stopResize.current?.();
    const pointer = event.pointerId; const startY = event.clientY; const startHeight = timelineHeight;
    const move = (next: PointerEvent) => { if (next.pointerId === pointer) { setTimelineHeight(Math.max(120, Math.min(480, startHeight + startY - next.clientY))); setTimelineFull(false); } };
    const stop = (next?: PointerEvent) => { if (next && next.pointerId !== pointer) return; window.removeEventListener("pointermove", move); window.removeEventListener("pointerup", stop); window.removeEventListener("pointercancel", stop); stopResize.current = null; };
    stopResize.current = () => stop();
    window.addEventListener("pointermove", move); window.addEventListener("pointerup", stop); window.addEventListener("pointercancel", stop);
  };
  const changeCount = plans.reduce((sum, plan) => sum + (plan.items?.length ?? 0), 0);
  return { dir, revision, caps, model, modelError: built.error, empty, aspect, tab, setTab, mode, setMode, cues, captionsOn, setCaptionsOn, plans, plansError, changeCount, panelError, setPanelError, playheadS, setPlayheadS, selected, setSelected, fallback, timelineHeight, timelineFull, setTimelineFull, sideHidden, setSideHidden, editingBusy: busy || refreshing || session.busy, seekState, session, committed, onSeekPreview, onPosition, seek, currentCue, dragTimeline };
}
