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
