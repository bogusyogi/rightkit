/**
 * `@rightkit/react-shell/headless`: behaviour-only controls for hosts that keep their own markup, classes and styles.
 * Imports only `react`; no stylesheet and no styled component.
 */
export { useToggle, useSegmentedControl } from "./react/headless.js";
export type {
  HeadlessToggleProps,
  UseToggleOptions,
  HeadlessSegmentedOption,
  HeadlessSegmentedMode,
  UseSegmentedControlOptions,
  HeadlessSegmentedGroupProps,
  HeadlessSegmentedOptionProps,
  HeadlessSegmentedControl,
} from "./react/headless.js";

export { playTone, type ToneSpec } from "./tone.js";

export {
  featureReelControlState,
  featureReelReducedMotionChanged,
  initialFeatureAutoAdvance,
  initialFeatureReelPlayback,
  setFeatureReelTransientPause,
  shouldFeatureReelAutoAdvance,
  toggleFeatureReelAutoAdvance,
  useFeatureReelPlayback,
  type FeatureReelControlLabels,
  type FeatureReelPlayback,
} from "./feature-reel.js";
export { focusConnectedOrPageDestination, focusPageDestination, type FocusPageOptions } from "./focus-page.js";
export { useHashRoute, type UseHashRouteOptions } from "./hash-route.js";
export {
  DEFAULT_SPLASH_STALL_SECS,
  splashMessageForWait,
  useSplashClock,
  useSplashStall,
  useSplashWallpaper,
  useTickingValue,
  type SplashStage,
  type SplashStatus,
  type UseSplashStallOptions,
} from "./splash.js";
