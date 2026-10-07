import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
  type ReactNode,
  type Ref,
  type SyntheticEvent,
} from "react";
import { useMediaAdapters, type MediaAdapters } from "./adapters.js";
import { resolveIcon, type IconName } from "./icons.js";

export interface MediaTrack {
  src: string;
  label?: string;
  srclang?: string;
  /** "captions" | "subtitles" — defaults to "subtitles". */
  kind?: string;
  default?: boolean;
}

export interface MediaPlayerProps {
  /** Media source: absolute path, URL, or app route — resolved via `resolveSrc`. */
  src: string;
  kind: "audio" | "video";
  /** Opaque media id handed to `preparePlayback` for compat conversion. */
  playbackId?: string;
  poster?: string;
  tracks?: MediaTrack[];
  /** Shorthand for a single captions track (transcript). */
  captionsUrl?: string | null;
  title?: string;
  /** Small info line under the control bar (e.g. "320×180 · 4s"). */
  info?: string;
  /** URL handed to "Open original"; defaults to `src`. */
  openUrl?: string;
  compact?: boolean;
  autoPlay?: boolean;
  mediaRef?: Ref<HTMLMediaElement>;
  onTimeChange?: (seconds: number) => void;
  onReady?: () => void;
  onPlaybackError?: () => void;
  /** Open the source in the OS default player (error fallback). */
  onOpenExternal?: () => void;
  /** Optional action shown after a load failure (e.g. generate a proxy). */
  onProxy?: () => void;
  proxyLabel?: string;
  /** Show the audio artwork / face panel (audio only). Default true. */
  showArtwork?: boolean;
  /** Show the "Open original" action (toolbar link and error-card button). Default true. */
  showOpenOriginal?: boolean;
  /** Replace any built-in icon (a `ReactNode`; `null` renders nothing). */
  icons?: Partial<Record<IconName, ReactNode>>;
  /** Selectable playback speeds. Default `DEFAULT_MEDIA_SPEEDS`. */
  speeds?: number[];
  /** "cycle" = one button stepping through `speeds`; "menu" = native `<select>`. Default "cycle". */
  speedControl?: "cycle" | "menu";
  /** Override any user-visible string. */
  labels?: Partial<MediaPlayerLabels>;
  /** Replace the error card entirely. */
  renderError?: (ctx: MediaPlayerErrorContext) => ReactNode;
  adapters?: MediaAdapters;
}

export interface MediaPlayerErrorContext {
  message: string;
  retry: () => void;
  openOriginal?: () => void;
}

/** Every user-visible / accessible string the player renders. */
export interface MediaPlayerLabels {
  mediaPlayer: string;
  loading: string;
  buffering: string;
  converting: string;
  play: string;
  pause: string;
  replay: string;
  replayFromStart: string;
  mute: string;
  unmute: string;
  seek: string;
  volume: string;
  playbackTime: string;
  /** Prefix of the speed control's accessible name; the speed is appended ("Playback speed 1.5×"). */
  playbackSpeed: string;
  subtitles: string;
  fullscreen: string;
  exitFullscreen: string;
  openOriginal: string;
  openInDefaultPlayer: string;
  errorTitle: string;
  errorUnavailable: string;
  errorOriginalUnchanged: string;
  retry: string;
  proxy: string;
  errorUnsupported: string;
  errorNetwork: string;
  errorDecode: string;
  errorLoad: string;
  errorCompatCopy: string;
  /** Prefix for runtime failures; the cause is appended. */
  playbackFailed: string;
  fullscreenFailed: string;
}

export const DEFAULT_MEDIA_PLAYER_LABELS: MediaPlayerLabels = {
  mediaPlayer: "Media player",
  loading: "Loading media…",
  buffering: "Buffering…",
  converting: "Preparing compatible playback copy…",
  play: "Play",
  pause: "Pause",
  replay: "Replay",
  replayFromStart: "Replay from start",
  mute: "Mute",
  unmute: "Unmute",
  seek: "Seek",
  volume: "Volume",
  playbackTime: "Playback time",
  playbackSpeed: "Playback speed",
  subtitles: "Subtitles",
  fullscreen: "Fullscreen",
  exitFullscreen: "Exit fullscreen",
  openOriginal: "Open original",
  openInDefaultPlayer: "Open in default player",
  errorTitle: "Preview failed",
  errorUnavailable: "Media unavailable, corrupt or unsupported.",
  errorOriginalUnchanged: "The original file is unchanged.",
  retry: "Retry",
  proxy: "Preview proxy",
  errorUnsupported: "Unsupported codec or container for this platform player.",
  errorNetwork: "Network error while loading media.",
  errorDecode: "Could not decode this media file.",
  errorLoad: "Could not load this media file.",
  errorCompatCopy:
    "Unsupported codec or corrupt media. Compatibility copy could not be decoded.",
  playbackFailed: "Playback could not start:",
  fullscreenFailed: "Fullscreen could not start:",
};

export const DEFAULT_MEDIA_SPEEDS: readonly number[] = [0.5, 0.75, 1, 1.25, 1.5, 2];

/** Next speed after `current` in `speeds` (wraps; unknown current -> first). */
export function nextMediaSpeed(speeds: readonly number[], current: number): number {
  if (speeds.length === 0) return current;
  return speeds[(speeds.indexOf(current) + 1) % speeds.length] ?? current;
}

/** Merge partial label overrides over the defaults (undefined values are ignored). */
export function resolveMediaLabels(overrides?: Partial<MediaPlayerLabels>): MediaPlayerLabels {
  const out = { ...DEFAULT_MEDIA_PLAYER_LABELS };
  if (overrides)
    for (const key of Object.keys(overrides) as (keyof MediaPlayerLabels)[]) {
      const value = overrides[key];
      if (typeof value === "string") out[key] = value;
    }
  return out;
}

// Only one playback at a time across all MediaPlayer instances on the page.
let currentExclusive: HTMLMediaElement | null = null;

const KEY_HANDLED = [
  " ", "k", "ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown", "m", "f", "r", "j", "l",
];

export function mediaClock(seconds: number): string {
  const whole = Math.floor(Number.isFinite(seconds) ? Math.max(0, seconds) : 0);
  const hours = Math.floor(whole / 3600);
  const minutes = Math.floor((whole % 3600) / 60);
  const rest = String(whole % 60).padStart(2, "0");
  return hours
    ? `${hours}:${String(minutes).padStart(2, "0")}:${rest}`
    : `${minutes}:${rest}`;
}

export function mediaErrorMessage(
  el: HTMLMediaElement,
  labels: MediaPlayerLabels = DEFAULT_MEDIA_PLAYER_LABELS,
): string {
  // HTMLMediaElement error codes are stable constants; compare numerically so
  // this also works where the MediaError global is absent (tests, pre-DOM).
  switch (el.error?.code) {
    case 4:
      return labels.errorUnsupported;
    case 2:
      return labels.errorNetwork;
    case 3:
      return labels.errorDecode;
    default:
      return labels.errorLoad;
  }
}

type Status = "loading" | "ready" | "buffering" | "converting" | "error";

/**
 * Native WebView media playback with a source-preserving compatibility
 * fallback: when the element can't decode `src` and the app provided a
 * `preparePlayback` adapter, a compat copy is requested once (`converting`
 * state) and played instead. Video bytes are remuxed, never re-encoded — see
 * the `rightkit-playback` crate.
 */
export function MediaPlayer(props: MediaPlayerProps) {
  const {
    src,
    kind,
    playbackId,
    poster,
    tracks,
    captionsUrl,
    title,
    info,
    openUrl,
    compact,
    autoPlay,
    mediaRef,
    onTimeChange,
    onReady,
    onPlaybackError,
    onOpenExternal,
    onProxy,
    proxyLabel,
    showArtwork = true,
    showOpenOriginal = true,
    icons,
    speedControl = "cycle",
    renderError,
  } = props;
  const adapters = useMediaAdapters(props.adapters);
  const labels = useMemo(() => resolveMediaLabels(props.labels), [props.labels]);
  const speeds = props.speeds && props.speeds.length > 0 ? props.speeds : DEFAULT_MEDIA_SPEEDS;
  const icon = (name: IconName, size?: number) => resolveIcon(name, icons, size);

  const element = useRef<HTMLMediaElement | null>(null);
  const container = useRef<HTMLDivElement | null>(null);
  const alive = useRef(true);
  const request = useRef<string | null>(null);
  const attempted = useRef(false);
  const nativeFullscreenRef = useRef(false);
  const [source, setSource] = useState(src);
  // Bumped whenever the requested media changes; async compat-copy results
  // from an earlier generation are discarded instead of replacing the new source.
  const sourceGeneration = useRef(0);
  const lastIdentity = useRef(`${src}\u0000${playbackId ?? ""}`);
  const [nativeFullscreen, setNativeFullscreen] = useState(false);
  const [elementFullscreen, setElementFullscreen] = useState(false);
  const [status, setStatus] = useState<Status>("loading");
  const [error, setError] = useState("");
  const [playing, setPlaying] = useState(false);
  const [ended, setEnded] = useState(false);
  const [time, setTime] = useState(0);
  const [duration, setDuration] = useState(0);
  const [volume, setVolume] = useState(1);
  const [muted, setMuted] = useState(false);
  const [speed, setSpeed] = useState(() => (speeds.includes(1) ? 1 : (speeds[0] ?? 1)));
  const [retry, setRetry] = useState(0);

  const effectiveTracks: MediaTrack[] = (() => {
    const list = [...(tracks ?? [])];
    if (captionsUrl) {
      list.unshift({ src: captionsUrl, label: "Transcript", srclang: "und", kind: "captions", default: true });
    }
    return list;
  })();
  const [captions, setCaptions] = useState(
    () => Boolean(captionsUrl) || effectiveTracks.some((t) => t.default),
  );

  const resolveSrc = useCallback(
    (path: string) => {
      try {
        if (/^[a-z][a-z0-9+.-]*:\/\//i.test(path) || path.startsWith("blob:")) return path;
        return adapters.resolveSrc?.(path) ?? path;
      } catch {
        return path;
      }
    },
    [adapters],
  );

  const attach = (el: HTMLMediaElement | null) => {
    if (el) element.current = el;
    if (typeof mediaRef === "function") mediaRef(el);
    else if (mediaRef) mediaRef.current = el;
  };

  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
      if (nativeFullscreenRef.current) {
        nativeFullscreenRef.current = false;
        void Promise.resolve(adapters.setFullscreen?.(false)).catch(() => {});
      }
      const el = element.current;
      if (el) {
        if (currentExclusive === el) currentExclusive = null;
        el.pause();
        el.removeAttribute("src");
        el.load(); // Release decoder, file handle & buffered data on unmount.
      }
      if (typeof mediaRef === "function") mediaRef(null);
      else if (mediaRef) mediaRef.current = null;
      if (request.current) void adapters.cancelPlayback?.(request.current);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [mediaRef]);

  // New media without a remount: drop everything that belonged to the old
  // one (compat copy, error, progress) so the player never shows/plays it.
  useLayoutEffect(() => {
    const identity = `${src}\u0000${playbackId ?? ""}`;
    if (lastIdentity.current === identity) return;
    lastIdentity.current = identity;
    sourceGeneration.current += 1;
    if (request.current) void adapters.cancelPlayback?.(request.current);
    request.current = null;
    attempted.current = false;
    setSource(src);
    setStatus("loading");
    setError("");
    setPlaying(false);
    setEnded(false);
    setTime(0);
    setDuration(0);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [src, playbackId]);

  // Release the decoder of whichever element is being replaced (the unmount
  // cleanup above only covers the final one).
  useEffect(() => {
    const el = element.current;
    // A cleanup that ran without a re-render (StrictMode's mount → cleanup →
    // mount, or a remounted effect) stripped `src`; React will not re-apply an
    // unchanged attribute, so restore it here or the element never loads.
    if (el && source && !el.getAttribute("src")) {
      el.setAttribute("src", resolveSrc(source));
      el.load();
    }
    return () => {
      if (!el) return;
      if (currentExclusive === el) currentExclusive = null;
      el.pause();
      el.removeAttribute("src");
      el.load();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [source, retry]);

  useEffect(() => {
    const handleEscape = (event: KeyboardEvent) => {
      if (event.key === "Escape" && nativeFullscreenRef.current) {
        nativeFullscreenRef.current = false;
        setNativeFullscreen(false);
        void Promise.resolve(adapters.setFullscreen?.(false)).catch((cause) =>
          setError(String(cause)),
        );
      }
    };
    document.addEventListener("keydown", handleEscape);
    return () => document.removeEventListener("keydown", handleEscape);
  }, [adapters]);

  useEffect(() => {
    const handleFullscreenChange = () =>
      setElementFullscreen(document.fullscreenElement === container.current);
    document.addEventListener("fullscreenchange", handleFullscreenChange);
    return () => document.removeEventListener("fullscreenchange", handleFullscreenChange);
  }, []);

  const syncTracks = useCallback(() => {
    const el = element.current;
    if (el)
      for (const track of Array.from(el.textTracks))
        track.mode = captions ? "showing" : "disabled";
  }, [captions]);
  useEffect(syncTracks, [captions, syncTracks]);

  const report = (event: SyntheticEvent<HTMLMediaElement>) => {
    const el = event.currentTarget;
    setTime(el.currentTime);
    onTimeChange?.(el.currentTime);
  };
  const ready = () => {
    const el = element.current;
    if (!el) return;
    setDuration(Number.isFinite(el.duration) ? el.duration : 0);
    el.volume = volume;
    el.muted = muted;
    el.playbackRate = speed;
    syncTracks();
    setStatus("ready");
    onReady?.();
  };

  const fail = async () => {
    const code = element.current?.error?.code;
    if (code === 1) return; // Abort during deliberate source replacement.
    if (!attempted.current && adapters.preparePlayback) {
      attempted.current = true;
      const generation = sourceGeneration.current;
      setStatus("converting");
      const requestId = `playback-${crypto.randomUUID()}`;
      request.current = requestId;
      try {
        const preview = await adapters.preparePlayback({
          id: playbackId ?? src,
          requestId,
        });
        if (!alive.current || generation !== sourceGeneration.current) return;
        setTime(0);
        setStatus("loading");
        setSource(preview);
        return;
      } catch (cause) {
        if (!alive.current || generation !== sourceGeneration.current) return;
        setError(String(cause));
      }
    } else {
      setError(
        attempted.current
          ? labels.errorCompatCopy
          : element.current
            ? mediaErrorMessage(element.current, labels)
            : labels.errorLoad,
      );
    }
    setStatus("error");
    setPlaying(false);
    onPlaybackError?.();
  };

  const toggle = useCallback(async () => {
    const el = element.current;
    if (!el) return;
    if (!el.paused) {
      el.pause();
      return;
    }
    if (el.ended) el.currentTime = 0;
    try {
      await el.play();
    } catch (cause) {
      setError(`${labels.playbackFailed} ${String(cause)}`);
    }
  }, [labels]);

  const seekTo = useCallback((t: number) => {
    const el = element.current;
    if (!el || !Number.isFinite(el.duration)) return;
    el.currentTime = Math.min(Math.max(0, t), el.duration || 0);
  }, []);

  const stepVolume = useCallback((d: number) => {
    const el = element.current;
    if (!el) return;
    el.muted = false;
    el.volume = Math.min(1, Math.max(0, el.volume + d));
  }, []);

  const replay = useCallback(() => {
    const el = element.current;
    if (!el) return;
    el.currentTime = 0;
    setTime(0);
    setEnded(false);
    void el.play().catch((cause) => setError(`${labels.playbackFailed} ${String(cause)}`));
  }, [labels]);

  const fullscreen = useCallback(async () => {
    try {
      if (nativeFullscreenRef.current) {
        nativeFullscreenRef.current = false;
        setNativeFullscreen(false);
        await adapters.setFullscreen?.(false);
        return;
      }
      if (document.fullscreenElement) {
        await document.exitFullscreen();
        return;
      }
      try {
        const target = container.current;
        if (target?.requestFullscreen) {
          await target.requestFullscreen();
          return;
        }
        const video = element.current as
          | (HTMLVideoElement & { webkitEnterFullscreen?: () => void })
          | null;
        if (kind === "video" && video?.webkitEnterFullscreen) {
          video.webkitEnterFullscreen();
          return;
        }
      } catch {
        /* WKWebView can expose DOM fullscreen without enabling it. */
      }
      if (!adapters.setFullscreen) return;
      await adapters.setFullscreen(true);
      if (!alive.current) {
        await adapters.setFullscreen(false);
        return;
      }
      nativeFullscreenRef.current = true;
      setNativeFullscreen(true);
      setError("");
    } catch (cause) {
      setError(`${labels.fullscreenFailed} ${String(cause)}`);
    }
  }, [adapters, kind, labels]);

  const onKey = useCallback(
    (e: ReactKeyboardEvent) => {
      if (!KEY_HANDLED.includes(e.key)) return;
      const target = e.target as HTMLElement | null;
      if (target?.closest?.("input, textarea, select, [contenteditable='true']")) return;
      e.preventDefault();
      e.stopPropagation();
      const el = element.current;
      if (!el) return;
      switch (e.key) {
        case " ":
        case "k":
          void toggle();
          break;
        case "ArrowLeft":
          seekTo(el.currentTime - 5);
          break;
        case "ArrowRight":
          seekTo(el.currentTime + 5);
          break;
        case "j":
          seekTo(el.currentTime - 10);
          break;
        case "l":
          seekTo(el.currentTime + 10);
          break;
        case "ArrowUp":
          stepVolume(0.1);
          break;
        case "ArrowDown":
          stepVolume(-0.1);
          break;
        case "m":
          el.muted = !el.muted;
          break;
        case "f":
          void fullscreen();
          break;
        case "r":
          replay();
          break;
      }
    },
    [toggle, seekTo, stepVolume, fullscreen, replay],
  );

  const doRetry = () => {
    const el = element.current;
    setError("");
    setStatus("loading");
    setEnded(false);
    if (el) el.load();
    else setRetry((n) => n + 1);
  };

  const cycleSpeed = () => {
    const el = element.current;
    if (!el) return;
    chooseSpeed(nextMediaSpeed(speeds, el.playbackRate));
  };
  const chooseSpeed = (next: number) => {
    const el = element.current;
    if (el) el.playbackRate = next;
    setSpeed(next);
  };

  const errorContext: MediaPlayerErrorContext = {
    message: error || labels.errorUnavailable,
    retry: doRetry,
    openOriginal: showOpenOriginal ? onOpenExternal : undefined,
  };

  const events = {
    onTimeUpdate: report,
    onSeeked: report,
    onLoadedMetadata: ready,
    onCanPlay: ready,
    onDurationChange: () => {
      const value = element.current?.duration;
      if (value != null && Number.isFinite(value)) setDuration(value);
    },
    onPlay: () => {
      const el = element.current;
      if (el) {
        if (currentExclusive && currentExclusive !== el) currentExclusive.pause();
        currentExclusive = el;
      }
      setPlaying(true);
      setEnded(false);
      setError("");
    },
    onPause: () => {
      setPlaying(false);
      if (element.current && currentExclusive === element.current) currentExclusive = null;
    },
    onEnded: () => {
      setPlaying(false);
      setEnded(true);
      if (element.current && currentExclusive === element.current) currentExclusive = null;
    },
    onWaiting: () => setStatus("buffering"),
    onPlaying: () => setStatus("ready"),
    onVolumeChange: (event: SyntheticEvent<HTMLMediaElement>) => {
      setVolume(event.currentTarget.volume);
      setMuted(event.currentTarget.muted);
    },
    onError: () => void fail(),
  };

  const enabled = status === "ready" || status === "buffering";
  const trackElements = effectiveTracks.map((t, i) => (
    <track
      key={i}
      kind={(t.kind as "subtitles" | "captions") ?? "subtitles"}
      src={resolveSrc(t.src)}
      label={t.label}
      srcLang={t.srclang}
      default={t.default}
      onLoad={syncTracks}
    />
  ));

  const audioElement = (
    <audio
      key={`${source}:${retry}`}
      ref={attach}
      preload="metadata"
      autoPlay={autoPlay}
      src={resolveSrc(source)}
      {...events}
    >
      {trackElements}
    </audio>
  );

  const mediaElement =
    status !== "error" && status !== "converting" ? (
      kind === "video" ? (
        <video
          key={`${source}:${retry}`}
          ref={attach}
          className="rk-media-video"
          preload="metadata"
          autoPlay={autoPlay}
          playsInline
          poster={poster}
          src={resolveSrc(source)}
          onClick={() => void toggle()}
          onDoubleClick={() => void fullscreen()}
          {...events}
        >
          {trackElements}
        </video>
      ) : showArtwork ? (
        <div
          className="rk-media-audio-face"
          onClick={() => void toggle()}
          role="button"
          aria-label={playing ? labels.pause : labels.play}
        >
          <span className="rk-media-audio-glyph" aria-hidden="true">
            &#9834;
          </span>
          {audioElement}
        </div>
      ) : (
        audioElement
      )
    ) : null;

  return (
    <div
      ref={container}
      className={`rk-media rk-media-${kind}${compact ? " compact" : ""}${nativeFullscreen ? " is-native-fullscreen" : ""}`}
      data-player-status={status}
      tabIndex={0}
      onKeyDown={onKey}
      role="group"
      aria-label={title ?? labels.mediaPlayer}
    >
      {mediaElement}
      {status !== "ready" && status !== "error" && (
        <p role="status" className="rk-media-status">
          {status === "converting"
            ? labels.converting
            : status === "buffering"
              ? labels.buffering
              : labels.loading}
        </p>
      )}
      {status !== "error" && (
        <div className="rk-media-controls" onClick={(e) => e.stopPropagation()}>
          <button
            type="button"
            className="rk-media-btn"
            aria-label={playing ? labels.pause : ended ? labels.replay : labels.play}
            disabled={!enabled}
            onClick={() => (ended ? replay() : void toggle())}
          >
            {playing ? icon("pause") : ended ? icon("replay") : icon("play")}
          </button>
          <button
            type="button"
            className="rk-media-btn"
            aria-label={labels.replayFromStart}
            disabled={!enabled}
            onClick={replay}
          >
            {icon("replay", 15)}
          </button>
          <output className="rk-media-time" aria-label={labels.playbackTime}>
            {mediaClock(time)} / {mediaClock(duration)}
          </output>
          <input
            className="rk-media-seek"
            type="range"
            aria-label={labels.seek}
            min={0}
            max={duration || 1}
            step={0.1}
            value={Math.min(time, duration || 1)}
            disabled={!enabled || !duration}
            onChange={(e) => {
              const value = Number(e.target.value);
              if (element.current) element.current.currentTime = value;
              setTime(value);
              onTimeChange?.(value);
            }}
          />
          <button
            type="button"
            className="rk-media-btn"
            aria-label={muted ? labels.unmute : labels.mute}
            onClick={() => {
              if (element.current) element.current.muted = !muted;
            }}
          >
            {muted || volume === 0 ? icon("mute") : icon("volume")}
          </button>
          <input
            className="rk-media-vol"
            type="range"
            aria-label={labels.volume}
            min={0}
            max={1}
            step={0.05}
            value={muted ? 0 : volume}
            onChange={(e) => {
              const el = element.current;
              if (!el) return;
              el.muted = false;
              el.volume = Number(e.target.value);
            }}
          />
          {speedControl === "menu" ? (
            <select
              className="rk-media-speed-menu"
              aria-label={`${labels.playbackSpeed} ${speed}×`}
              value={speed}
              onChange={(e) => chooseSpeed(Number(e.target.value))}
            >
              {(speeds.includes(speed) ? speeds : [speed, ...speeds]).map((value) => (
                <option key={value} value={value}>
                  {value}×
                </option>
              ))}
            </select>
          ) : (
            <button
              type="button"
              className="rk-media-btn rk-media-speed"
              aria-label={`${labels.playbackSpeed} ${speed}×`}
              onClick={cycleSpeed}
            >
              {speed}×
            </button>
          )}
          {kind === "video" && effectiveTracks.length > 0 && (
            <button
              type="button"
              className="rk-media-btn"
              aria-label={labels.subtitles}
              aria-pressed={captions}
              onClick={() => setCaptions(!captions)}
            >
              CC
            </button>
          )}
          {kind === "video" && (
            <button
              type="button"
              className="rk-media-btn"
              aria-label={
                nativeFullscreen || elementFullscreen ? labels.exitFullscreen : labels.fullscreen
              }
              disabled={!enabled}
              onClick={() => void fullscreen()}
            >
              {icon("fullscreen")}
            </button>
          )}
          {showOpenOriginal && (openUrl !== undefined || src) ? (
            <a
              className="rk-media-btn"
              href={openUrl ?? resolveSrc(src)}
              target="_blank"
              rel="noreferrer"
              aria-label={labels.openOriginal}
              title={labels.openOriginal}
            >
              {icon("external")}
            </a>
          ) : null}
        </div>
      )}
      {info ? <div className="rk-media-info">{info}</div> : null}
      {error && status !== "error" && (
        <p role="alert" className="rk-media-error">
          {error}
        </p>
      )}
      {status === "error" &&
        (renderError ? (
          renderError(errorContext)
        ) : (
          <div className="rk-media-fallback" role="alert">
            <b>{labels.errorTitle}</b>
            <span>{errorContext.message}</span>
            <span className="rk-media-fallback-note">{labels.errorOriginalUnchanged}</span>
            <div className="rk-media-fallback-actions">
              <button type="button" className="rk-media-btn" onClick={doRetry}>
                {labels.retry}
              </button>
              {onProxy ? (
                <button type="button" className="rk-media-btn" onClick={onProxy}>
                  {proxyLabel ?? labels.proxy}
                </button>
              ) : null}
              {errorContext.openOriginal && (
                <button
                  type="button"
                  className="rk-media-btn"
                  onClick={errorContext.openOriginal}
                >
                  {icon("external")} {labels.openInDefaultPlayer}
                </button>
              )}
            </div>
          </div>
        ))}
    </div>
  );
}
