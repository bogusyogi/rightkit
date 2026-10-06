import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
  type Ref,
  type SyntheticEvent,
} from "react";
import { useMediaAdapters, type MediaAdapters } from "./adapters.js";
import {
  ExternalIcon,
  FullscreenIcon,
  MuteIcon,
  PauseIcon,
  PlayIcon,
  ReplayIcon,
  VolumeIcon,
} from "./icons.js";

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
  adapters?: MediaAdapters;
}

// Only one playback at a time across all MediaPlayer instances on the page.
let currentExclusive: HTMLMediaElement | null = null;

const SPEEDS = [0.5, 0.75, 1, 1.25, 1.5, 2];
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

export function mediaErrorMessage(el: HTMLMediaElement): string {
  // HTMLMediaElement error codes are stable constants; compare numerically so
  // this also works where the MediaError global is absent (tests, pre-DOM).
  switch (el.error?.code) {
    case 4:
      return "Unsupported codec or container for this platform player.";
    case 2:
      return "Network error while loading media.";
    case 3:
      return "Could not decode this media file.";
    default:
      return "Could not load this media file.";
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
  } = props;
  const adapters = useMediaAdapters(props.adapters);

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
  const [speed, setSpeed] = useState(1);
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
    return () => {
      if (!el) return;
      if (currentExclusive === el) currentExclusive = null;
      el.pause();
      el.removeAttribute("src");
      el.load();
    };
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
          ? "Unsupported codec or corrupt media. Compatibility copy could not be decoded."
          : element.current
            ? mediaErrorMessage(element.current)
            : "Could not load this media file.",
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
      setError(`Playback could not start: ${String(cause)}`);
    }
  }, []);

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
    void el.play().catch((cause) => setError(`Playback could not start: ${String(cause)}`));
  }, []);

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
      setError(`Fullscreen could not start: ${String(cause)}`);
    }
  }, [adapters, kind]);

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
    const next = SPEEDS[(SPEEDS.indexOf(el.playbackRate) + 1) % SPEEDS.length] ?? 1;
    el.playbackRate = next;
    setSpeed(next);
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
      ) : (
        <div
          className="rk-media-audio-face"
          onClick={() => void toggle()}
          role="button"
          aria-label={playing ? "Pause" : "Play"}
        >
          <span className="rk-media-audio-glyph" aria-hidden="true">
            &#9834;
          </span>
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
        </div>
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
      aria-label={title ?? "Media player"}
    >
      {mediaElement}
      {status !== "ready" && status !== "error" && (
        <p role="status" className="rk-media-status">
          {status === "converting"
            ? "Preparing compatible playback copy…"
            : status === "buffering"
              ? "Buffering…"
              : "Loading media…"}
        </p>
      )}
      {status !== "error" && (
        <div className="rk-media-controls" onClick={(e) => e.stopPropagation()}>
          <button
            type="button"
            className="rk-media-btn"
            aria-label={playing ? "Pause" : ended ? "Replay" : "Play"}
            disabled={!enabled}
            onClick={() => (ended ? replay() : void toggle())}
          >
            {playing ? <PauseIcon /> : ended ? <ReplayIcon /> : <PlayIcon />}
          </button>
          <button
            type="button"
            className="rk-media-btn"
            aria-label="Replay from start"
            disabled={!enabled}
            onClick={replay}
          >
            <ReplayIcon size={15} />
          </button>
          <output className="rk-media-time" aria-label="Playback time">
            {mediaClock(time)} / {mediaClock(duration)}
          </output>
          <input
            className="rk-media-seek"
            type="range"
            aria-label="Seek"
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
            aria-label={muted ? "Unmute" : "Mute"}
            onClick={() => {
              if (element.current) element.current.muted = !muted;
            }}
          >
            {muted || volume === 0 ? <MuteIcon /> : <VolumeIcon />}
          </button>
          <input
            className="rk-media-vol"
            type="range"
            aria-label="Volume"
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
          <button
            type="button"
            className="rk-media-btn rk-media-speed"
            aria-label={`Playback speed ${speed}×`}
            onClick={cycleSpeed}
          >
            {speed}×
          </button>
          {kind === "video" && effectiveTracks.length > 0 && (
            <button
              type="button"
              className="rk-media-btn"
              aria-label="Subtitles"
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
                nativeFullscreen || elementFullscreen ? "Exit fullscreen" : "Fullscreen"
              }
              disabled={!enabled}
              onClick={() => void fullscreen()}
            >
              <FullscreenIcon />
            </button>
          )}
          {openUrl !== undefined || src ? (
            <a
              className="rk-media-btn"
              href={openUrl ?? resolveSrc(src)}
              target="_blank"
              rel="noreferrer"
              aria-label="Open original"
              title="Open original"
            >
              <ExternalIcon />
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
      {status === "error" && (
        <div className="rk-media-fallback" role="alert">
          <b>Preview failed</b>
          <span>{error || "Media unavailable, corrupt or unsupported."}</span>
          <span className="rk-media-fallback-note">
            The original file is unchanged.
          </span>
          <div className="rk-media-fallback-actions">
            <button type="button" className="rk-media-btn" onClick={doRetry}>
              Retry
            </button>
            {onProxy ? (
              <button type="button" className="rk-media-btn" onClick={onProxy}>
                {proxyLabel ?? "Preview proxy"}
              </button>
            ) : null}
            {onOpenExternal && (
              <button type="button" className="rk-media-btn" onClick={onOpenExternal}>
                <ExternalIcon /> Open in default player
              </button>
            )}
          </div>
        </div>
      )}
    </div>
  );
}
