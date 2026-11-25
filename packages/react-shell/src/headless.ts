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
  useFeatureReelFrame,
  useFeatureReelPreload,
  preloadImagesWhenIdle,
  type FeatureReelControlLabels,
  type FeatureReelPlayback,
  type UseFeatureReelFrameOptions,
  type PreloadImagesOptions,
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
  formatMenuBarClock,
  formatTaskbarClock,
  type ClockFormatOptions,
  type SplashStage,
  type SplashStatus,
  type UseSplashStallOptions,
} from "./splash.js";
export { createFieldStore, useField, type FieldStore } from "./field-store.js";
export { createErrorHumanizer, humanizeError, type ErrorRule, type HumanizeErrorOptions } from "./humanize-error.js";
export { usePaletteFocusRestore, type UsePaletteFocusRestoreOptions } from "./palette-focus-restore.js";
export {
  useHubSetupGate,
  useStartupRead,
  useThemeChoiceSave,
  useWindowCustomEvent,
  type UseHubSetupGateOptions,
  type UseStartupReadOptions,
  type UseThemeChoiceSaveOptions,
} from "./hub-boot.js";
export {
  buildCatalogView,
  useCatalogView,
  useLoadedList,
  type CatalogEntryLike,
  type CatalogView,
  type CatalogViewOptions,
} from "./catalog-view.js";
