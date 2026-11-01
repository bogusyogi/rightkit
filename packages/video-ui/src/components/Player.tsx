import { Button, IconButton, Select, Slider } from "@rightkit/react-shell/react";
import { invoke } from "@tauri-apps/api/core";
import { useCallback, useEffect, useRef, useState } from "react";
import { errorMessage } from "../lib/engine";
import {
  type PlaybackMeta,
  playbackClose,
  playbackControl,
  playbackFrame,
  playbackOpen,
  PLAYBACK_REFRESHED,
} from "../lib/media";
import { formatSeconds } from "../lib/project";
import { Icon, timecode } from "../ui";

/**
 * Preview-only output gain (0.0..=1.0, 0 mutes). Drives the native transport's
 * audio callback; export/render code never reads it.
 */
const playbackSetGain = (value: number) => invoke<number>("playback_set_gain", { value });

/** Draw one binary frame reply onto the canvas; returns the decoded header. */
function drawFrame(canvas: HTMLCanvasElement, buf: ArrayBuffer | Uint8Array) {
  const bytes = buf instanceof Uint8Array ? buf : new Uint8Array(buf);
  if (bytes.length < 24) return null;
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const w = view.getUint32(0, true);
  const h = view.getUint32(4, true);
  const frame = view.getUint32(8, true);
  const total = view.getUint32(12, true);
  const pts = view.getFloat64(16, true);
  const rgba = new Uint8ClampedArray(w * h * 4);
  rgba.set(bytes.subarray(24, 24 + w * h * 4));
  if (canvas.width !== w) canvas.width = w;
  if (canvas.height !== h) canvas.height = h;
  canvas.getContext("2d")?.putImageData(new ImageData(rgba, w, h), 0, 0);
  return { frame, total, pts };
}

/**
 * Native playback surface: frames are decoded and composited in Rust (FFmpeg +
 * wgpu) and delivered here as RGBA; the browser never decodes the timeline.
 * `dir` plays the canonical project revision.
 */
export function NativePlayer({
  dir,
  label,
  seekRequest,
  onStatus,
  onError,
}: {
  dir?: string;
  label?: string;
  seekRequest?: { ms: number; n: number } | null;
  onStatus?: (meta: PlaybackMeta) => void;
  onError?: (message: string) => void;
}) {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const [meta, setMeta] = useState<PlaybackMeta | null>(null);
  const [state, setState] = useState<"loading" | "ready" | "error">("loading");
  const [error, setError] = useState<string | null>(null);
  const drawing = useRef(false);

  const apply = useCallback(
    (m: PlaybackMeta) => {
      setMeta(m);
      onStatus?.(m);
    },
    [onStatus],
  );

  const draw = useCallback(async () => {
    if (drawing.current) return;
    drawing.current = true;
    try {
      const buf = await playbackFrame();
      const canvas = canvasRef.current;
      if (canvas) drawFrame(canvas, buf);
    } catch {
      // transient (session replaced mid-frame); the status loop surfaces errors
    } finally {
      drawing.current = false;
    }
  }, []);

  // Open/replace the session for this source.
  useEffect(() => {
    let cancelled = false;
    setState("loading");
    setError(null);
    (async () => {
      try {
        const m = await playbackOpen({ dir });
        if (cancelled) return;
        apply(m);
        setState("ready");
        await draw();
      } catch (e) {
        if (!cancelled) {
          const msg = errorMessage(e);
          setError(msg);
          setState("error");
          onError?.(msg);
        }
      }
    })();
    return () => {
      cancelled = true;
      void playbackClose().catch(() => undefined);
    };
  }, [dir, apply, draw, onError]);

  // External seek requests (e.g. timeline scrub).
  useEffect(() => {
    if (!seekRequest || state !== "ready") return;
    void playbackControl("seek", seekRequest.ms)
      .then(apply)
      .then(() => draw())
      .catch(() => undefined);
  }, [seekRequest, state, apply, draw]);

  // The session was rebuilt under a paused player (a colour grade changed): repaint.
  useEffect(() => {
    if (state !== "ready") return;
    const repaint = () => void draw().catch((e) => setError(errorMessage(e)));
    window.addEventListener(PLAYBACK_REFRESHED, repaint);
    return () => window.removeEventListener(PLAYBACK_REFRESHED, repaint);
  }, [state, draw]);

  // Frame pump while playing.
  useEffect(() => {
    if (state !== "ready" || !meta?.playing) return;
    let live = true;
    const pump = async () => {
      const { playbackStatus } = await import("../lib/media");
      while (live) {
        await draw();
        const still = await playbackStatus().catch(() => null);
        if (!still || !still.playing) {
          if (still) apply(still);
          return;
        }
        apply(still);
        if (!live) return;
        await new Promise((r) => setTimeout(r, 8));
      }
    };
    void pump();
    return () => {
      live = false;
    };
  }, [state, meta?.playing, draw, apply]);

  const control = (cmd: string, value?: number, value2?: number) =>
    playbackControl(cmd, value, value2)
      .then(apply)
      .then(() => draw())
      .catch((e) => setError(errorMessage(e)));

  if (state === "error") {
    return (
      <p role="alert" className="cr-muted m-0 text-sm">
        {label ?? dir}: native playback failed — {error}
      </p>
    );
  }

  const pos = (meta?.position_ms ?? 0) / 1000;
  const dur = (meta?.duration_ms ?? 0) / 1000;

  return (
    <div className="cr-player">
      <canvas
        ref={canvasRef}
        style={{
          display: "block",
          width: "100%",
          maxHeight: "420px",
          background: "black",
          opacity: state === "loading" ? 0.3 : 1,
        }}
      />
      {state === "loading" && (
        <p role="status" className="cr-muted m-0 text-sm">
          Preparing native playback…
        </p>
      )}
      <div className="seek-row cr-dim cr-mono" style={{ alignItems: "center", gap: 8 }}>
        <Button
          type="button"
          disabled={state !== "ready"}
          onClick={() => void control("toggle")}
        >
          {meta?.playing ? "Pause" : "Play"}
        </Button>
        <Button
          type="button"
          disabled={state !== "ready"}
          onClick={() => void control("step", -1)}
        >
          -1f
        </Button>
        <Button
          type="button"
          disabled={state !== "ready"}
          onClick={() => void control("step", 1)}
        >
          +1f
        </Button>
        <Slider
          label="Seek"
          min={0}
          max={Math.max(dur, 0.001)}
          step={0.001}
          value={pos}
          disabled={state !== "ready"}
          onChange={(value) => void control("seek", value * 1000)}
          className="flex-1"
        />
        <Select
          label="Speed"
          value={String(meta?.speed ?? 1)}
          disabled={state !== "ready"}
          onChange={(value) => void control("speed", Number(value))}
          options={[0.5, 1, 1.5, 2, 4].map((speed) => ({
            value: String(speed),
            label: `${speed}x`,
          }))}
        />
        <Button
          type="button"
          disabled={state !== "ready"}
          aria-pressed={!!meta?.loop}
          onClick={() =>
            void control(
              "loop",
              meta?.loop ? -1 : 0,
              meta?.loop ? 0 : (meta?.duration_ms ?? 0),
            )
          }
        >
          Loop
        </Button>
      </div>
      <div className="seek-row cr-dim cr-mono">
        <span className="truncate" title={dir}>
          {label ?? "Timeline preview"}
        </span>
        <span>
          {formatSeconds(pos)} / {formatSeconds(dur)}
          {meta ? ` · f${meta.frame}/${meta.total_frames}` : ""}
          {meta && meta.audio !== "on" && meta.kind === "timeline" ? " · muted" : ""}
        </span>
      </div>
    </div>
  );
}

/**
 * Preview host for the editor: revision-bound native playback of the project
 * timeline. `seek(revisionCursor, timeS)` scrubs to `timeS`.
 */
export function useTimelinePreview(dir: string | null, revision: number) {
  const [seekRequest, setSeekRequest] = useState<{ ms: number; n: number } | null>(
    null,
  );
  const host = {
    seek: (_revisionCursor: number, timeS: number) =>
      setSeekRequest({ ms: timeS * 1000, n: Date.now() }),
    slot: dir ? (
      <NativePlayer
        dir={dir}
        key={`${dir}:${revision}`}
        label="Timeline preview"
        seekRequest={seekRequest}
      />
    ) : null,
  };
  return host;
}

/**
 * Stage player for the Finished edit screen (§3.4): a letterboxed canvas fed
 * by the Rust compositor playback session for the CURRENT project revision,
 * plus the spec transport (play circle, ±1f, mono timecode, speed, loop,
 * scrubber). While the session opens — or if playback fails — it shows the
 * caller-provided fallback frame (`media.thumbnail`), never a black box.
 */
export function StagePlayer({
  dir,
  revision,
  seekRequest,
  captionsOn,
  caption,
  aspect,
  fallback,
  onPosition,
  onToggleCaptions,
}: {
  dir: string;
  /** Reopen the session when the revision changes (edit landed). */
  revision: number;
  seekRequest?: { ms: number; n: number } | null;
  captionsOn: boolean;
  caption?: string | null;
  aspect?: string | null;
  /** Asset URL of the nearest real frame to show while loading/on error. */
  fallback?: string | null;
  onPosition?: (ms: number) => void;
  onToggleCaptions: (v: boolean) => void;
}) {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const [meta, setMeta] = useState<PlaybackMeta | null>(null);
  const [state, setState] = useState<"loading" | "ready" | "error">("loading");
  const [error, setError] = useState<string | null>(null);
  const [volume, setVolume] = useState(1);
  const [muted, setMuted] = useState(false);
  /** Bumped by the Retry control to reopen the playback session for the same revision. */
  const [attempt, setAttempt] = useState(0);
  const drawing = useRef(false);

  const draw = useCallback(async () => {
    if (drawing.current) return;
    drawing.current = true;
    try {
      const buf = await playbackFrame();
      const canvas = canvasRef.current;
      if (canvas) drawFrame(canvas, buf);
    } finally {
      drawing.current = false;
    }
  }, []);

  const apply = useCallback(
    (m: PlaybackMeta) => {
      setMeta(m);
      onPosition?.(m.position_ms);
    },
    [onPosition],
  );

  // Gain is process-wide in the native transport, so it survives session reopen.
  // A failed call leaves the previous gain in place; the stage keeps playing.
  const setGain = (gain: number) => {
    void playbackSetGain(gain).catch(() => undefined);
  };
  const onVolume = (v: number) => {
    setVolume(v);
    setMuted(false);
    setGain(v);
  };
  const toggleMute = () => {
    if (muted) {
      const restored = volume > 0 ? volume : 1;
      setVolume(restored);
      setMuted(false);
      setGain(restored);
    } else {
      setMuted(true);
      setGain(0);
    }
  };

  // One session per (dir, revision, attempt): an applied edit changes the programme,
  // and Retry reopens the session after a playback error.
  useEffect(() => {
    let cancelled = false;
    setState("loading");
    setError(null);
    (async () => {
      try {
        const m = await playbackOpen({ dir, height: 720 });
        if (cancelled) return;
        apply(m);
        setState("ready");
        await draw();
      } catch (e) {
        if (!cancelled) {
          setError(errorMessage(e));
          setState("error");
        }
      }
    })();
    return () => {
      cancelled = true;
      void playbackClose().catch(() => undefined);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [dir, revision, attempt]);

  // Seeks run in request order, and only the newest one's reply moves the playhead:
  // a late reply from an earlier keystroke must not snap it back.
  const seekChain = useRef<Promise<unknown>>(Promise.resolve());
  const latestSeek = useRef<unknown>(null);
  useEffect(() => {
    if (!seekRequest || state !== "ready") return;
    latestSeek.current = seekRequest;
    seekChain.current = seekChain.current
      .catch(() => undefined)
      .then(() => playbackControl("seek", seekRequest.ms))
      .then(async (m) => {
        if (latestSeek.current !== seekRequest) return;
        apply(m);
        await draw();
      })
      .catch((e) => setError(errorMessage(e)));
  }, [seekRequest, state, apply, draw]);

  // The session was rebuilt under a paused player (a colour grade changed): repaint.
  useEffect(() => {
    if (state !== "ready") return;
    const repaint = () => void draw().catch((e) => setError(errorMessage(e)));
    window.addEventListener(PLAYBACK_REFRESHED, repaint);
    return () => window.removeEventListener(PLAYBACK_REFRESHED, repaint);
  }, [state, draw]);

  // Frame pump while playing.
  useEffect(() => {
    if (state !== "ready" || !meta?.playing) return;
    let live = true;
    const pump = async () => {
      const { playbackStatus } = await import("../lib/media");
      while (live) {
        try {
          await draw();
        } catch (e) {
          // A persistent decode failure (e.g. missing media) is honest UI:
          // surface it rather than leaving a black canvas.
          if (live) {
            setError(errorMessage(e));
            setState("error");
          }
          return;
        }
        const still = await playbackStatus().catch(() => null);
        if (!still || !still.playing) {
          if (still) apply(still);
          return;
        }
        apply(still);
        if (!live) return;
        await new Promise((r) => setTimeout(r, 8));
      }
    };
    void pump();
    return () => {
      live = false;
    };
  }, [state, meta?.playing, draw, apply]);

  const control = (cmd: string, value?: number, value2?: number) =>
    playbackControl(cmd, value, value2)
      .then(apply)
      .then(() => draw())
      .catch((e) => setError(errorMessage(e)));

  const pos = (meta?.position_ms ?? 0) / 1000;
  const dur = (meta?.duration_ms ?? 0) / 1000;
  const ready = state === "ready";

  const tidyError = (e: string | null) => {
    if (!e) return "unknown error";
    const last = e.split("\n").filter(Boolean).pop() ?? e;
    return last.replace(/\S*\/([^\s/]+)/g, "$1");
  };

  return (
    <>
      <div className="fe-stage" data-preview-state={state} data-preview-error={error ?? undefined}>
        {fallback && state !== "ready" && (
          <img className="fe-stage-fallback" src={fallback} alt="" draggable={false} />
        )}
        <canvas
          ref={canvasRef}
          style={{ display: ready ? "block" : fallback ? "none" : "block" }}
        />
        {state === "loading" && !fallback && (
          <div className="fe-stage__loading">
            <span role="status" className="cr-muted">Loading preview…</span>
          </div>
        )}
        {state === "error" && (
          <div className="fe-stage__loading" role="alert" title={error ?? undefined}>
            Preview failed — {tidyError(error)}
          </div>
        )}
        {state === "error" && (
          <Button
            type="button"
            style={{ position: "absolute", left: "50%", bottom: 12, transform: "translateX(-50%)" }}
            onClick={() => setAttempt((n) => n + 1)}
          >
            Retry
          </Button>
        )}
        <span className="fe-stage__tag">Final edit</span>
        <span className="fe-stage__chips">
          <button
            type="button"
            className="rk-badge"
            aria-pressed={captionsOn}
            onClick={() => onToggleCaptions(!captionsOn)}
          >
            Captions {captionsOn ? "on" : "off"}
          </button>
          {aspect && <span className="rk-badge">{aspect}</span>}
        </span>
        {captionsOn && caption && <div className="fe-stage__caption">{caption}</div>}
      </div>

      <div className="fe-transport">
        <div className="fe-transport__row">
          <IconButton
            label="-1 frame"
            disabled={!ready}
            onClick={() => void control("step", -1)}
          ><Icon name="step-back" /></IconButton>
          <button
            type="button"
            className="fe-play"
            aria-label={meta?.playing ? "Pause" : "Play"}
            disabled={!ready}
            onClick={() => void control("toggle")}
          >
            <Icon name={meta?.playing ? "pause" : "play"} />
          </button>
          <IconButton
            label="+1 frame"
            disabled={!ready}
            onClick={() => void control("step", 1)}
          ><Icon name="step-forward" /></IconButton>
          <span className="fe-transport__tc">
            <strong>{timecode(meta?.position_ms ?? 0)}</strong> / {timecode(meta?.duration_ms ?? 0)}
          </span>
          <span className="fe-transport__right">
            <Select
              className="w-auto h-[26px]!"
              label="Playback speed"
              value={String(meta?.speed ?? 1)}
              disabled={!ready}
              onChange={(value) => void control("speed", Number(value))}
              options={[0.5, 1, 1.5, 2].map((speed) => ({
                value: String(speed),
                label: `${speed}×`,
              }))}
            />
            <IconButton
              label="Loop"
              pressed={!!meta?.loop}
              disabled={!ready}
              onClick={() =>
                void control("loop", meta?.loop ? -1 : 0, meta?.loop ? 0 : meta?.duration_ms ?? 0)
              }
            ><Icon name="repeat" /></IconButton>
            <IconButton
              label="Mute"
              pressed={muted}
              onClick={toggleMute}
            ><Icon name={muted || volume === 0 ? "volume-x" : "volume"} /></IconButton>
            <Slider
              label="Volume"
              min={0}
              max={1}
              step={0.01}
              value={muted ? 0 : volume}
              onChange={onVolume}
              className="w-[72px]!"
            />
          </span>
        </div>
        <Slider
          label="Scrub"
          min={0}
          max={Math.max(dur, 0.001)}
          step={0.001}
          value={pos}
          disabled={!ready}
          onChange={(value) => void control("seek", value * 1000)}
        />
      </div>
    </>
  );
}

/** Shape consumed by the editor lane (`TimelineView` / `TimelineEditorView`). */
export type PreviewHost = ReturnType<typeof useTimelinePreview>;
