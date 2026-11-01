export type FeatureReelPlayback = {
  autoAdvanceEnabled: boolean;
  pausedByHover: boolean;
  pausedByFocus: boolean;
};

export function initialFeatureAutoAdvance(prefersReducedMotion: boolean): boolean {
  return !prefersReducedMotion;
}

export function initialFeatureReelPlayback(
  prefersReducedMotion: boolean,
): FeatureReelPlayback {
  return {
    autoAdvanceEnabled: initialFeatureAutoAdvance(prefersReducedMotion),
    pausedByHover: false,
    pausedByFocus: false,
  };
}

export function toggleFeatureReelAutoAdvance(
  current: FeatureReelPlayback,
): FeatureReelPlayback {
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

export function featureReelControlState(autoAdvanceEnabled: boolean) {
  return autoAdvanceEnabled
    ? { label: "Pause feature carousel", paused: false }
    : { label: "Resume feature carousel", paused: true };
}
