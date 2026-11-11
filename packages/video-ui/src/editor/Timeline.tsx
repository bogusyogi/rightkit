/**
 * Finished-edit timeline: ruler, playhead,
 * selectable clips with trim/move/slip gestures in Edit mode, navigation-only
 * in Review mode. All pointer geometry is transient — the canonical timeline
 * updates only when `session.apply` commits a planned action and the host
 * refreshes `project.show`.
 *
 * Lanes follow the spec order: Footage, Titles, Captions (timeline-mapped
 * cues), Voice (waveform), Music, B-roll when a second video track exists.
 * Clip thumbnails come from `media.thumbnail`, waveforms from
 * `media.waveform` — both behind real engine data, never placeholders.
 *
 * Visual structure adapted from hyperframes' separable TimelineRuler pattern,
 * retargeted at the design tokens. The clip menu is the shell's ContextMenu.
 */
import { Button, ContextMenu, IconButton, Slider, cx, type MenuItem } from "@rightkit/react-shell/react";
import { Icon } from "../ui/index.js";
import { errorText } from "./adapter.js";
import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type PointerEvent as ReactPointerEvent,
  type ReactNode,
} from "react";
import { formatSeconds, newClipId } from "../lib/project.js";
import { assetUrl } from "../lib/asset.js";
import { PLAYBACK_REFRESHED, thumbnails, type CaptionCue } from "../lib/media.js";
import { engineCall } from "../lib/engine.js";
import { slipAudioClip, slipClip, splitAudioClip, splitClip, type EditAction } from "./actions.js";
import {
  formatTimelineTickLabel,
  generateTicks,
  getTimelineGridStep,
} from "./vendor/hyperframes/rulerGeometry.js";
import { clipEdges, planMove, planMoveMany, planTrim, snapTime, videoDropIndex } from "./gesture.js";
import type { EditorClip, EditorModel } from "./model.js";
import { supports, unsupported, type CapabilityReport } from "./capabilities.js";
import { useEditorKeys } from "./keys.js";
import type { EditSession } from "./adapter.js";

const LABEL_W = 72;
const RULER_H = 24;
const MIN_PPS = 4;
const MAX_PPS = 640;

export type PreviewSeek = (revisionCursor: number, timeS: number) => void;

type DragState =
  | { kind: "scrub"; pointerId: number }
  | {
      kind: "move";
      pointerId: number;
      clipIds: string[];
      startX: number;
      deltaS: number;
      dropIndex: number | null;
    }
  | {
      kind: "trim";
      pointerId: number;
      clip: EditorClip;
      edge: "in" | "out";
      startX: number;
      deltaS: number;
    }
  | { kind: "slip"; pointerId: number; clip: EditorClip; startX: number; deltaS: number };

/** One display lane: model clips, or synthetic caption cues. */
type DisplayLane =
  | { id: string; kind: "video" | "audio" | "overlay"; label: string; clips: EditorClip[] }
  | { id: string; kind: "captions"; label: string; cues: CaptionCue[] };

export function Timeline({
  model,
  session,
  caps,
  busy,
  mode,
  cues,
  dir,
  playheadS,
  onPlayhead,
  onCommitted,
  onSeek,
  onSelect,
  onMenuAction,
}: {
  model: EditorModel;
  session: EditSession;
  caps: CapabilityReport | null;
  busy: boolean;
  /** `review` = navigate only (click/scrub to seek). `edit` = full gestures. */
  mode: "review" | "edit";
  /** Timeline-mapped caption cues (output timebase). */
  cues: CaptionCue[];
  /** Project dir for thumbnail/waveform calls. */
  dir: string;
  playheadS: number;
  onPlayhead: (timeS: number) => void;
  onCommitted: () => void;
  onSeek: PreviewSeek;
  onSelect: (ids: ReadonlySet<string>) => void;
  onMenuAction?: (clip: EditorClip) => MenuItem[];
}) {
  const viewportRef = useRef<HTMLDivElement | null>(null);
  const [pps, setPps] = useState(48);
  const [snapping, setSnapping] = useState(true);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [drag, setDrag] = useState<DragState | null>(null);
  const [menu, setMenu] = useState<{ x: number; y: number; clip: EditorClip } | null>(null);
  const [gestureError, setGestureError] = useState<string | null>(null);
  const shuttle = useRef({ dir: 0, mag: 1 });
  const editing = mode === "edit";

  const duration = Math.max(model.durationS, 1);
  const contentW = Math.max(duration * pps, 240);
  // Ruler gains :ff frame labels once a frame is wider than ~3px
  // (4 frames / 12px past that zoom, per §1 Typography).
  const showFrames = pps >= model.fps * 3;
  const { major, minor } = useMemo(
    () => generateTicks(duration, pps, showFrames ? model.fps : undefined),
    [duration, pps, model.fps, showFrames],
  );
  const majorInterval = major.length > 1 ? major[1]! - major[0]! : 1;
  const gridStep = useMemo(
    () => getTimelineGridStep(duration, pps, model.fps),
    [duration, pps, model.fps],
  );

  const lanes = useMemo<DisplayLane[]>(() => {
    const out: DisplayLane[] = [];
    let videoSeen = 0;
    for (const l of model.lanes.filter((l) => l.kind === "video")) {
      videoSeen += 1;
      out.push({ id: l.id, kind: "video", label: videoSeen === 1 ? "Footage" : `B-roll`, clips: l.clips });
    }
    for (const l of model.lanes.filter((l) => l.kind === "overlay")) {
      out.push({ id: l.id, kind: "overlay", label: "Titles", clips: l.clips });
    }
    if (cues.length > 0) out.push({ id: "captions", kind: "captions", label: "Captions", cues });
    for (const l of model.lanes.filter((l) => l.kind === "audio")) {
      const role = l.label.toLowerCase();
      const label =
        role === "narr" || role === "narration" || role === "dialogue" || role === "voice"
          ? "Voice"
          : role === "music"
            ? "Music"
            : l.label;
      out.push({ id: l.id, kind: "audio", label, clips: l.clips });
    }
    return out;
  }, [model, cues]);

  // Drop selection that no longer exists after a refresh (agent edits, undo).
  useEffect(() => {
    setSelected((prev) => {
      const live = new Set(model.lanes.flatMap((l) => l.clips.map((c) => c.id)));
      const next = new Set([...prev].filter((id) => live.has(id)));
      return next.size === prev.size && [...next].every((id) => prev.has(id)) ? prev : next;
    });
  }, [model]);

  useEffect(() => onSelect(selected), [selected, onSelect]);

  const seekTo = useCallback(
    (t: number) => {
      const clamped = Math.max(0, Math.min(duration, t));
      onPlayhead(clamped);
      onSeek(model.revisionCursor, clamped);
    },
    [duration, model.revisionCursor, onPlayhead, onSeek],
  );

  const timeAtClientX = useCallback(
    (clientX: number): number => {
      const el = viewportRef.current;
      if (!el) return 0;
      const rect = el.getBoundingClientRect();
      return (clientX - rect.left + el.scrollLeft - LABEL_W) / pps;
    },
    [pps],
  );

  const commit = useCallback(
    async (actions: EditAction[], label: string) => {
      if (actions.length === 0) return;
      setGestureError(null);
      try {
        await session.apply(actions, label);
        onCommitted();
      } catch (e) {
        setGestureError(errorText(e));
        onCommitted(); // still refresh: engine is the source of truth
      }
    },
    [session, onCommitted],
  );

  // ---- pointer handling -------------------------------------------------

  const startScrub = (e: ReactPointerEvent) => {
    if (e.button !== 0) return;
    (e.currentTarget as Element).setPointerCapture(e.pointerId);
    setDrag({ kind: "scrub", pointerId: e.pointerId });
    seekTo(timeAtClientX(e.clientX));
  };

  const startClipGesture = (e: ReactPointerEvent, clip: EditorClip, edge?: "in" | "out") => {
    if (e.button !== 0 || busy) return;
    e.stopPropagation();
    if (!editing) {
      // Review mode: the timeline navigates; a clip click seeks to it.
      seekTo(clip.start);
      return;
    }
    (e.currentTarget as Element).setPointerCapture(e.pointerId);
    const nextSelected = e.shiftKey
      ? new Set(selected).add(clip.id)
      : selected.has(clip.id)
        ? selected
        : new Set([clip.id]);
    setSelected(nextSelected);
    if (edge) {
      setDrag({ kind: "trim", pointerId: e.pointerId, clip, edge, startX: e.clientX, deltaS: 0 });
    } else if (e.altKey && clip.ref.kind !== "title") {
      if (!supports("edit.slip", caps)) {
        setGestureError(unsupported("edit.slip").reason);
        return;
      }
      setDrag({ kind: "slip", pointerId: e.pointerId, clip, startX: e.clientX, deltaS: 0 });
    } else {
      setDrag({
        kind: "move",
        pointerId: e.pointerId,
        clipIds: [...nextSelected],
        startX: e.clientX,
        deltaS: 0,
        dropIndex: null,
      });
    }
  };

  const moveGesture = (e: ReactPointerEvent) => {
    if (!drag || e.pointerId !== drag.pointerId) return;
    const t = timeAtClientX(e.clientX);
    if (drag.kind === "scrub") {
      seekTo(t);
      return;
    }
    if (drag.kind === "trim" || drag.kind === "slip") {
      setDrag({ ...drag, deltaS: (e.clientX - drag.startX) / pps });
      return;
    }
    const first = model.lanes.flatMap((l) => l.clips).find((c) => c.id === drag.clipIds[0]);
    const raw = (e.clientX - drag.startX) / pps;
    const exclude = new Set(drag.clipIds);
    const targetStart = Math.max(0, (first?.start ?? 0) + raw);
    const snapped = snapping
      ? snapTime(targetStart, gridStep, clipEdges(model, exclude), 0.08)
      : targetStart;
    const deltaS = Math.abs(snapped - targetStart) <= 0.08
      ? snapped - (first?.start ?? 0)
      : raw;
    const dropIndex =
      first?.ref.kind === "video"
        ? videoDropIndex(
            model.lanes.find((l) => l.id === (first.ref as { trackId: string }).trackId)!,
            first.id,
            t,
          )
        : null;
    setDrag({ ...drag, deltaS, dropIndex });
  };

  const endGesture = (e: ReactPointerEvent) => {
    if (!drag || e.pointerId !== drag.pointerId) return;
    const done = drag;
    setDrag(null);
    if (done.kind === "scrub") {
      seekTo(timeAtClientX(e.clientX));
      return;
    }
    if (done.kind === "trim") {
      if (Math.abs(done.deltaS) < 1 / model.fps / 2) return;
      void commit(planTrim(model, done.clip, done.edge, done.deltaS), `Trim ${done.clip.label}`);
      return;
    }
    if (done.kind === "slip") {
      const step = gridStep > 0 ? gridStep : 1 / model.fps;
      const d = Math.abs(done.deltaS) < step / 2 ? 0 : Math.round(done.deltaS / step) * step;
      if (d === 0) return;
      void commit(
        [done.clip.ref.kind === "audio" ? slipAudioClip(done.clip.id, d) : slipClip(done.clip.id, d)],
        `Slip ${done.clip.label}`,
      );
      return;
    }
    const clips = done.clipIds
      .map((id) => model.lanes.flatMap((l) => l.clips).find((c) => c.id === id))
      .filter((c): c is EditorClip => Boolean(c));
    const first = clips[0];
    if (!first) return;
    if (first.ref.kind === "video" && done.dropIndex != null) {
      void commit(planMove(model, first, { index: done.dropIndex }), `Move ${first.label}`);
      return;
    }
    if (Math.abs(done.deltaS) < gridStep / 4 && Math.abs(done.deltaS) < 0.02) return;
    void commit(planMoveMany(model, clips, done.deltaS), "Move clips");
  };

  // ---- keyboard ----------------------------------------------------------

  // The chords live in ./keys (registered on the shell's shortcut registry); these are the actions,
  // active while focus is inside the timeline.
  const rootRef = useRef<HTMLDivElement | null>(null);
  const reportKeyError = (err: unknown) => setGestureError(err instanceof Error ? err.message : String(err));
  const step = (dir: number) => {
    shuttle.current = { dir: 0, mag: 1 };
    seekTo(playheadS + dir / model.fps);
  };
  const shuttleBy = (d: number) => {
    const mag = shuttle.current.dir === d ? Math.min(shuttle.current.mag * 4, 16) : 1;
    shuttle.current = { dir: d, mag };
    seekTo(playheadS + (d * mag) / model.fps);
  };
  useEditorKeys(rootRef, {
    undo: () => void session.undo().then(onCommitted).catch(reportKeyError),
    redo: () => void session.redo().then(onCommitted).catch(reportKeyError),
    redoY: () => void session.redo().then(onCommitted).catch(() => undefined),
    stepBack: () => step(-1),
    stepForward: () => step(1),
    shuttleBack: () => shuttleBy(-1),
    pause: () => {
      shuttle.current = { dir: 0, mag: 1 };
    },
    shuttleForward: () => shuttleBy(1),
    ...(editing
      ? {
          remove: () => (selected.size > 0 ? void removeSelected() : false),
          split: () => splitAtPlayhead(),
        }
      : {}),
  });

  /** Splits `clip` at the playhead. `at` is seconds into the clip's own span. */
  const splitClipAt = (clip: EditorClip): EditAction[] => {
    // The playhead can sit between frames (a seek the player has not answered yet);
    // a cut must land on a frame, so it goes to the nearest one.
    const at = Math.round((playheadS - clip.start) * model.fps) / model.fps;
    if (at <= 0 || at >= clip.duration) return [];
    const id = newClipId();
    if (clip.ref.kind === "video") return [splitClip(clip.id, at, id)];
    if (clip.ref.kind === "audio") return [splitAudioClip(clip.id, at, id)];
    return [];
  };

  /** Split the selected clips — or, with no selection, whatever the playhead crosses. */
  const splitAtPlayhead = () => {
    if (!supports("edit.split", caps)) {
      setGestureError(unsupported("edit.split").reason);
      return;
    }
    const targets =
      selected.size > 0
        ? [...selected]
        : model.lanes
            .flatMap((l) => l.clips)
            .filter((c) => playheadS > c.start && playheadS < c.start + c.duration)
            .map((c) => c.id);
    const actions = targets.flatMap((id) => {
      const clip = model.lanes.flatMap((l) => l.clips).find((c) => c.id === id);
      return clip ? splitClipAt(clip) : [];
    });
    if (actions.length === 0) {
      setGestureError("Playhead is not inside a clip");
      return;
    }
    void commit(actions, "Split at playhead");
  };

  // The shell menu restarts its highlight when `items` changes identity, so the list is built once per open.
  // Row actions read the current render's closures through a ref.
  const menuOps = useRef({
    split: (_clip: EditorClip) => {},
    remove: (_clip: EditorClip) => {},
  });
  menuOps.current = {
    split: (clip) => {
      setSelected(new Set([clip.id]));
      const actions = splitClipAt(clip);
      if (actions.length === 0) {
        setGestureError("Playhead is not inside the clip");
        return;
      }
      void commit(actions, `Split ${clip.label}`);
    },
    remove: (clip) => {
      setSelected(new Set([clip.id]));
      void removeClipNow(clip);
    },
  };
  const menuItems = useMemo(
    () =>
      menu
        ? clipMenuItems(menu.clip, model, caps, {
            split: () => menuOps.current.split(menu.clip),
            remove: () => menuOps.current.remove(menu.clip),
            extras: onMenuAction?.(menu.clip) ?? [],
          })
        : [],
    [menu, model, caps, onMenuAction],
  );

  const removeSelected = async () => {
    const ids = [...selected];
    const actions: EditAction[] = ids.flatMap((id) => {
      const clip = model.lanes.flatMap((l) => l.clips).find((c) => c.id === id);
      if (!clip) return [];
      if (clip.ref.kind === "video") return [{ type: "remove_clip", clip_id: id }];
      if (clip.ref.kind === "audio") return [{ type: "remove_audio_clip", clip_id: id }];
      return [{ type: "remove_title", clip_id: id }];
    });
    setSelected(new Set());
    await commit(actions, "Remove clips");
  };

  const zoom = (next: number, anchorClientX?: number) => {
    const el = viewportRef.current;
    const clamped = Math.max(MIN_PPS, Math.min(MAX_PPS, next));
    if (el && anchorClientX != null) {
      const rect = el.getBoundingClientRect();
      const t = timeAtClientX(anchorClientX);
      el.scrollLeft = t * clamped + LABEL_W - (anchorClientX - rect.left);
    }
    setPps(clamped);
  };

  const fit = () => {
    const el = viewportRef.current;
    if (!el) return;
    const w = el.clientWidth - LABEL_W - 8;
    setPps(Math.max(MIN_PPS, Math.min(MAX_PPS, w / duration)));
  };

  const undo = () =>
    session
      .undo()
      .then(onCommitted)
      .catch((err) => setGestureError(err instanceof Error ? err.message : String(err)));
  const redo = () =>
    session
      .redo()
      .then(onCommitted)
      .catch((err) => setGestureError(err instanceof Error ? err.message : String(err)));

  // ---- render ------------------------------------------------------------

  const playheadX = LABEL_W + playheadS * pps;

  return (
    <div ref={rootRef} style={{ display: "flex", flexDirection: "column", minHeight: 0, flex: 1 }}>
      <div className="fe-tl__bar">
        <IconButton label="Select" pressed={editing}><Icon name="mouse-pointer" /></IconButton>
        <IconButton
          label="Split at playhead"
          disabled={!editing || !supports("edit.split", caps)}
          onClick={splitAtPlayhead}
        ><Icon name="scissors" /></IconButton>
        <IconButton
          label="Undo"
          disabled={!model.canUndo || session.busy}
          onClick={() => void undo()}
        ><Icon name="undo" /></IconButton>
        <IconButton
          label="Redo"
          disabled={!model.canRedo || session.busy}
          onClick={() => void redo()}
        ><Icon name="redo" /></IconButton>
        <IconButton
          label={snapping ? "Snapping on" : "Snapping off"}
          pressed={snapping}
          onClick={() => setSnapping((s) => !s)}
        ><Icon name="magnet" /></IconButton>
        <div className="fe-tl__zoom">
          <Slider
            className="w-28!"
            label="Timeline zoom"
            min={MIN_PPS}
            max={MAX_PPS}
            value={pps}
            onChange={(value) => zoom(value)}
          />
          <Button size="sm" variant="ghost" onClick={fit}>
            Fit
          </Button>
        </div>
      </div>
      {gestureError && (
        <p role="alert" className="cr-muted" style={{ margin: 0, padding: "0 var(--s4, 16px)", fontSize: 12 }}>
          {gestureError}
        </p>
      )}

      <div
        ref={viewportRef}
        tabIndex={0}
        role="region"
        aria-label="Timeline"
        className="fe-tl__viewport"
        onWheel={(e) => {
          if (e.ctrlKey || e.metaKey) {
            e.preventDefault();
            zoom(pps * (e.deltaY > 0 ? 0.85 : 1.18), e.clientX);
          }
        }}
      >
        <div style={{ width: contentW + LABEL_W, position: "relative", minHeight: "100%" }}>
          {/* Ruler — sticky top. */}
          <div
            className="fe-tl__ruler"
            onPointerDown={startScrub}
            onPointerMove={moveGesture}
            onPointerUp={endGesture}
            role="slider"
            aria-label="Playhead"
            aria-valuemin={0}
            aria-valuemax={Math.round(duration * model.fps)}
            aria-valuenow={Math.round(playheadS * model.fps)}
            aria-valuetext={formatSeconds(playheadS)}
          >
            <div
              style={{
                position: "sticky",
                left: 0,
                zIndex: 8,
                width: LABEL_W,
                flex: "none",
                background: "var(--rk-plane)",
              }}
            />
            <div style={{ position: "relative", width: contentW, height: RULER_H }}>
              {minor.map((t) => (
                <div key={`m${t}`} className="fe-tl__ruler-tick" style={{ left: t * pps - 0.5 }} />
              ))}
              {major.map((t) => (
                <div key={`M${t}`} className="fe-tl__ruler-major" style={{ left: t * pps - 0.5 }}>
                  <span>
                    {showFrames
                      ? `${formatTimelineTickLabel(t, duration, majorInterval)}:${String(
                          Math.round((t * model.fps) % model.fps),
                        ).padStart(2, "0")}`
                      : formatTimelineTickLabel(t, duration, majorInterval)}
                  </span>
                  <i />
                </div>
              ))}
            </div>
          </div>

          {/* Lanes */}
          {lanes.map((lane) => (
            <div key={`${lane.kind}:${lane.id}`} className="fe-lane" role="row" aria-label={`${lane.label} lane`}>
              <div className="fe-lane__label">
                <span>{lane.label}</span>
              </div>
              <div
                className="fe-lane__track"
                style={{ width: contentW }}
                onPointerDown={mode === "review" ? startScrub : undefined}
                onPointerMove={mode === "review" ? moveGesture : undefined}
                onPointerUp={mode === "review" ? endGesture : undefined}
              >
                {lane.kind === "captions"
                  ? lane.cues.map((c, i) => (
                      <button
                        key={i}
                        type="button"
                        className="fe-clip fe-clip--caption"
                        style={{ left: (c.start_ms / 1000) * pps, width: ((c.end_ms - c.start_ms) / 1000) * pps }}
                        title={c.text}
                        onClick={() => seekTo(c.start_ms / 1000)}
                      >
                        <span className="fe-clip__label">{c.text}</span>
                      </button>
                    ))
                  : lane.clips.map((clip) => {
                      const isSel = selected.has(clip.id);
                      const moveDelta =
                        drag?.kind === "move" && drag.clipIds.includes(clip.id) ? drag.deltaS : 0;
                      const trimDelta =
                        drag?.kind === "trim" && drag.clip.id === clip.id ? drag.deltaS : 0;
                      const left =
                        clip.start * pps + (clip.ref.kind === "video" ? 0 : moveDelta * pps);
                      const trimLeft =
                        drag?.kind === "trim" && drag.clip.id === clip.id && drag.edge === "in"
                          ? trimDelta
                          : 0;
                      const trimRight =
                        drag?.kind === "trim" && drag.clip.id === clip.id && drag.edge === "out"
                          ? trimDelta
                          : 0;
                      const w = Math.max((clip.duration + trimRight - trimLeft) * pps, 6);
                      return (
                        <div
                          key={clip.id}
                          role="button"
                          tabIndex={-1}
                          aria-label={`${clip.label} clip`}
                          aria-selected={isSel}
                          title={clip.label}
                          onPointerDown={(e) => startClipGesture(e, clip)}
                          onPointerMove={moveGesture}
                          onPointerUp={endGesture}
                          onContextMenu={(e) => {
                            e.preventDefault();
                            if (!editing) return;
                            setSelected(new Set([clip.id]));
                            setMenu({ x: e.clientX, y: e.clientY, clip });
                          }}
                          className={cx(
                            "fe-clip",
                            isSel && "fe-clip--sel",
                            session.busy && "fe-clip--pending",
                          )}
                          style={{ left: left + trimLeft * pps, width: w }}
                        >
                          {clip.ref.kind === "video" && (
                            <ClipThumbs
                              dir={dir}
                              source={clip.ref.clip.source}
                              inS={clip.ref.clip.source_in / 705_600_000}
                              outS={clip.ref.clip.source_out / 705_600_000}
                            />
                          )}
                          {clip.ref.kind === "audio" && supports("media.waveform", caps) && (
                            <ClipWave
                              dir={dir}
                              source={clip.ref.clip.source}
                              inS={clip.ref.clip.trim_in / 705_600_000}
                              outS={clip.ref.clip.trim_out / 705_600_000}
                              durS={model.sourceDurationS.get(clip.ref.clip.source) ?? 0}
                            />
                          )}
                          <span className="fe-clip__label">
                            {clip.ref.kind === "audio" && lane.label === "Music"
                              ? `${clip.label} ${Math.round(clip.ref.clip.gain / 1000)} dB`
                              : clip.label}
                          </span>
                          {editing && (
                            <>
                              <button
                                type="button"
                                aria-label={`Trim ${clip.label} in`}
                                className="fe-clip__handle fe-clip__handle--in"
                                onPointerDown={(e) => startClipGesture(e, clip, "in")}
                                onPointerMove={moveGesture}
                                onPointerUp={endGesture}
                              />
                              <button
                                type="button"
                                aria-label={`Trim ${clip.label} out`}
                                className="fe-clip__handle fe-clip__handle--out"
                                onPointerDown={(e) => startClipGesture(e, clip, "out")}
                                onPointerMove={moveGesture}
                                onPointerUp={endGesture}
                              />
                            </>
                          )}
                        </div>
                      );
                    })}
                {drag?.kind === "move" && drag.dropIndex != null && lane.kind === "video" && (
                  <DropIndicator lane={lane} index={drag.dropIndex} pps={pps} />
                )}
              </div>
            </div>
          ))}

          {/* Playhead: 2px accent line + 8px dot on the ruler. */}
          <div className="fe-tl__playhead" style={{ left: playheadX }} aria-hidden="true" />
        </div>
      </div>

      {menu && (
        <ContextMenu
          key={`${menu.x}:${menu.y}:${menu.clip.id}`}
          x={menu.x}
          y={menu.y}
          width={300}
          label="Clip actions"
          itemAs="button"
          items={menuItems}
          onClose={() => setMenu(null)}
        />
      )}
    </div>
  );

  async function removeClipNow(clip: EditorClip) {
    const action: EditAction =
      clip.ref.kind === "video"
        ? { type: "remove_clip", clip_id: clip.id }
        : clip.ref.kind === "audio"
          ? { type: "remove_audio_clip", clip_id: clip.id }
          : { type: "remove_title", clip_id: clip.id };
    await commit([action], `Remove ${clip.label}`);
  }
}

/** Thumbnail strip inside a footage clip (`media.thumbnail`, cached JPEGs). */
function ClipThumbs({
  dir,
  source,
  inS,
  outS,
}: {
  dir: string;
  source: string;
  inS: number;
  outS: number;
}) {
  const [urls, setUrls] = useState<string[]>([]);
  // The preview rebuild after a grade change also rebuilds the chain the frames are drawn through.
  const [graded, setGraded] = useState(0);
  useEffect(() => {
    const bump = () => setGraded((n) => n + 1);
    window.addEventListener(PLAYBACK_REFRESHED, bump);
    return () => window.removeEventListener(PLAYBACK_REFRESHED, bump);
  }, []);
  useEffect(() => {
    let cancelled = false;
    if (!(outS > inS)) return;
    const times = [0.1, 0.4, 0.7].map((f) => Math.round((inS + (outS - inS) * f) * 1000));
    thumbnails(dir, { source }, { times_ms: times, width: 96 })
      .then((r) => {
        if (!cancelled) setUrls(r.frames.map((f) => assetUrl(f.path)));
      })
      .catch(() => undefined);
    return () => {
      cancelled = true;
    };
  }, [dir, source, inS, outS, graded]);
  if (urls.length === 0) return null;
  return (
    <div className="fe-clip__thumbs" aria-hidden="true">
      {urls.map((u) => (
        <img key={u} src={u} alt="" draggable={false} />
      ))}
    </div>
  );
}

/** Waveform bars inside an audio clip (`media.waveform` peaks of the source). */
function ClipWave({
  dir,
  source,
  inS,
  outS,
  durS,
}: {
  dir: string;
  source: string;
  inS: number;
  outS: number;
  durS: number;
}) {
  const [peaks, setPeaks] = useState<number[] | null>(null);
  useEffect(() => {
    let cancelled = false;
    engineCall<{ peaks?: number[] }>("media.waveform", { dir, source, bars: 600 })
      .then((r) => !cancelled && setPeaks(r.peaks ?? []))
      .catch(() => !cancelled && setPeaks([]));
    return () => {
      cancelled = true;
    };
  }, [dir, source]);
  const bars = useMemo(() => {
    if (!peaks || peaks.length === 0 || !(durS > 0)) return [];
    const lo = Math.floor((inS / durS) * peaks.length);
    const hi = Math.max(lo + 1, Math.ceil((outS / durS) * peaks.length));
    const slice = peaks.slice(lo, hi);
    const n = 48;
    const out: number[] = [];
    for (let i = 0; i < n; i++) {
      const s = Math.floor((i / n) * slice.length);
      const e = Math.max(s + 1, Math.ceil(((i + 1) / n) * slice.length));
      out.push(Math.max(...slice.slice(s, e)));
    }
    return out;
  }, [peaks, inS, outS, durS]);
  if (bars.length === 0) return null;
  return (
    <div className="fe-clip__wave" aria-hidden="true">
      {bars.map((p, i) => (
        <i key={i} style={{ height: `${Math.max(12, Math.min(100, p * 140))}%` }} />
      ))}
    </div>
  );
}

function DropIndicator({
  lane,
  index,
  pps,
}: {
  lane: { clips: EditorClip[] };
  index: number;
  pps: number;
}) {
  const before = lane.clips[index - 1];
  const x = (before ? before.start + before.duration : 0) * pps;
  return (
    <div
      className="pointer-events-none absolute top-0 bottom-0 w-0.5"
      style={{ left: x, background: "var(--rk-accent)" }}
      aria-hidden="true"
    />
  );
}

/** A disabled row says why: the honest capability reason sits beside the label. */
function reasoned(label: string, reason: string | undefined): ReactNode {
  if (!reason) return label;
  return (
    <>
      {label}
      <span className="fe-menu__reason" title={reason}>
        {reason}
      </span>
    </>
  );
}

function clipMenuItems(
  clip: EditorClip,
  model: EditorModel,
  caps: CapabilityReport | null,
  ops: { split: () => void; remove: () => void; extras: MenuItem[] },
): MenuItem[] {
  const edits: MenuItem[] = [];
  if (clip.ref.kind === "video" || clip.ref.kind === "audio") {
    const canSplit = supports("edit.split", caps);
    const canSlip = supports("edit.slip", caps);
    edits.push(
      {
        id: "split",
        label: reasoned("Split at playhead", canSplit ? undefined : unsupported("edit.split").reason),
        shortcut: "S",
        disabled: !canSplit,
        run: ops.split,
      },
      {
        id: "slip",
        label: reasoned("Slip (Alt+drag clip)", canSlip ? undefined : unsupported("edit.slip").reason),
        disabled: !canSlip,
      },
    );
  }
  if (clip.ref.kind === "audio") {
    const videoClips = model.lanes.filter((l) => l.kind === "video").flatMap((l) => l.clips);
    if (videoClips.length > 0 && !clip.link) {
      edits.push({
        id: "link",
        label: reasoned(`Link to ${videoClips[0]!.label}`, "Use the Inspect tab's Link control"),
        disabled: true,
      });
    }
  }
  // Groups are divided by a separator; an empty group adds none.
  const groups = [edits, ops.extras, [{ id: "remove", label: "Remove", danger: true, run: ops.remove } satisfies MenuItem]];
  return groups
    .filter((group) => group.length > 0)
    .flatMap((group, index) => group.map((item, i) => (index > 0 && i === 0 ? { ...item, separatorBefore: true } : item)));
}
