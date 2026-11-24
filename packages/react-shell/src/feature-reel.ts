// Playback state of an auto-advancing carousel: auto-advance, hover and focus pauses, reduced motion.
// Behaviour only: the slides, markup and the control's wording belong to the host.
import { useCallback, useEffect, useState } from "react";

export type FeatureReelPlayback = {
  autoAdvanceEnabled: boolean;
  pausedByHover: boolean;
  pausedByFocus: boolean;
};

export type FeatureReelControlLabels = {
  /** Label of the control while the carousel plays (it pauses it). */
  pause: string;
  /** Label of the control while the carousel is paused (it resumes it). */
  resume: string;
};

export function initialFeatureAutoAdvance(prefersReducedMotion: boolean): boolean {
  return !prefersReducedMotion;
}

export function initialFeatureReelPlayback(prefersReducedMotion: boolean): FeatureReelPlayback {
  return {
    autoAdvanceEnabled: initialFeatureAutoAdvance(prefersReducedMotion),
    pausedByHover: false,
    pausedByFocus: false,
  };
}

export function toggleFeatureReelAutoAdvance(current: FeatureReelPlayback): FeatureReelPlayback {
  if (current.autoAdvanceEnabled) {
    return { ...current, autoAdvanceEnabled: false };
  }
  return {
    autoAdvanceEnabled: true,
    pausedByHover: false,
    pausedByFocus: false,
  };
}

export function featureReelReducedMotionChanged(
  current: FeatureReelPlayback,
  prefersReducedMotion: boolean,
): FeatureReelPlayback {
  return prefersReducedMotion && current.autoAdvanceEnabled
    ? { ...current, autoAdvanceEnabled: false }
    : current;
}

export function setFeatureReelTransientPause(
  current: FeatureReelPlayback,
  source: "hover" | "focus",
  paused: boolean,
): FeatureReelPlayback {
  const key = source === "hover" ? "pausedByHover" : "pausedByFocus";
  return current[key] === paused ? current : { ...current, [key]: paused };
}

export function shouldFeatureReelAutoAdvance(
  autoAdvanceEnabled: boolean,
  pausedByHover: boolean,
  pausedByFocus: boolean,
): boolean {
  return autoAdvanceEnabled && !pausedByHover && !pausedByFocus;
}

export function featureReelControlState(autoAdvanceEnabled: boolean, labels: FeatureReelControlLabels) {
  return autoAdvanceEnabled
    ? { label: labels.pause, paused: false }
    : { label: labels.resume, paused: true };
}

const REDUCED_MOTION_QUERY = "(prefers-reduced-motion: reduce)";

/**
 * The carousel's playback state with the reduced-motion preference wired in: starts paused when the user
 * prefers reduced motion and pauses when the preference turns on.
 */
export function useFeatureReelPlayback(labels: FeatureReelControlLabels) {
  const [playback, setPlayback] = useState(() => {
    const media = typeof window === "undefined" ? null : window.matchMedia?.(REDUCED_MOTION_QUERY);
    return initialFeatureReelPlayback(media?.matches ?? false);
  });
  const { autoAdvanceEnabled, pausedByHover, pausedByFocus } = playback;

  useEffect(() => {
    const media = window.matchMedia?.(REDUCED_MOTION_QUERY);
    if (!media) return;
    const onChange = (event: MediaQueryListEvent) => {
      setPlayback((current) => featureReelReducedMotionChanged(current, event.matches));
    };
    media.addEventListener("change", onChange);
    return () => media.removeEventListener("change", onChange);
  }, []);

  const setPaused = useCallback(
    (source: "hover" | "focus", paused: boolean) =>
      setPlayback((current) => setFeatureReelTransientPause(current, source, paused)),
    [],
  );
  const toggle = useCallback(() => setPlayback(toggleFeatureReelAutoAdvance), []);

  return {
    autoAdvanceEnabled,
    /** True while the carousel should advance on its timer. */
    advancing: shouldFeatureReelAutoAdvance(autoAdvanceEnabled, pausedByHover, pausedByFocus),
    /** `{ label, paused }` for the play/pause control: `aria-label` and `aria-pressed`. */
    control: featureReelControlState(autoAdvanceEnabled, labels),
    setPaused,
    toggle,
  };
}

/** Options of {@link useFeatureReelFrame}. */
export interface UseFeatureReelFrameOptions {
  /** Number of frames. The index wraps from the last back to 0. */
  count: number;
  /** True while the carousel should advance on its timer (`useFeatureReelPlayback().advancing`). */
  advancing: boolean;
  /** Time between automatic advances, in milliseconds. */
  intervalMs: number;
}

/**
 * The carousel's frame index with the auto-advance timer: starts at 0 and, while `advancing`, moves to the next
 * frame every `intervalMs` (wrapping). The timer restarts whenever `advancing` turns on, so a resumed carousel
 * waits a full interval. Returns `[index, setIndex]`; `setIndex` serves dots or arrow controls.
 */
export function useFeatureReelFrame(options: UseFeatureReelFrameOptions): [number, (next: number) => void] {
  const { count, advancing, intervalMs } = options;
  const [index, setIndex] = useState(0);
  useEffect(() => {
    if (!advancing || count <= 0) return;
    const id = window.setInterval(() => setIndex((current) => (current + 1) % count), intervalMs);
    return () => window.clearInterval(id);
  }, [advancing, count, intervalMs]);
  return [index, setIndex];
}

/** Options of {@link preloadImagesWhenIdle}. */
export interface PreloadImagesOptions {
  /** Longest wait for an idle moment, in milliseconds (`requestIdleCallback` timeout). Default 2000. */
  idleTimeoutMs?: number;
  /** Delay used where `requestIdleCallback` does not exist. Default 1200. */
  fallbackDelayMs?: number;
}

/**
 * Warms the browser image cache with `urls` during idle time, one `Image` each, skipping duplicates. The first
 * frame is the host's `<img>` and loads eagerly, so pass only the rest. Returns a function that cancels a
 * preload that has not started; call it from an effect cleanup.
 */
export function preloadImagesWhenIdle(urls: readonly string[], options: PreloadImagesOptions = {}): () => void {
  const { idleTimeoutMs = 2000, fallbackDelayMs = 1200 } = options;
  let cancelled = false;
  const preload = () => {
    if (cancelled) return;
    for (const url of new Set(urls)) {
      const img = new Image();
      img.src = url;
    }
  };
  let cancel: () => void;
  if (typeof window.requestIdleCallback === "function") {
    const id = window.requestIdleCallback(preload, { timeout: idleTimeoutMs });
    cancel = () => window.cancelIdleCallback(id);
  } else {
    const id = window.setTimeout(preload, fallbackDelayMs);
    cancel = () => window.clearTimeout(id);
  }
  return () => {
    cancelled = true;
    cancel();
  };
}

/**
 * {@link preloadImagesWhenIdle} on mount: the images after the first (the first renders eagerly) are warmed once
 * while idle. `urls` is read at mount only, as the slide list is static.
 */
export function useFeatureReelPreload(urls: readonly string[], options?: PreloadImagesOptions): void {
  useEffect(() => {
    const [first, ...rest] = urls;
    return preloadImagesWhenIdle(
      rest.filter((url) => url !== first),
      options,
    );
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
}
