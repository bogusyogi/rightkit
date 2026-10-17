/**
 * Headless entry: pure helpers and React hooks with no native detection. Nothing here imports
 * `@tauri-apps/plugin-os`, so apps without the plugin (or with no Tauri at all) can use it freely.
 */
export {
  formatChord,
  normalizePlatform,
  platformInitializationScript,
  type FormatChordOptions,
  type Platform,
} from "./format.js";
export {
  floatingPanelStyle,
  fitToViewport,
  type FitToViewportInput,
  type FitToViewportResult,
  type FloatingPanelInput,
  type FloatingPanelStyle,
} from "./geometry.js";
export * from "./toast.js";
export { blockNativeContextMenu } from "./context-menu.js";
export { relativeTimeFrom } from "./time.js";
export { attachFocusTrap, useFocusTrap, useLatest, useModalDismiss, type FocusTrapOptions } from "./focus.js";
export { attachDismiss, useDismiss, type DismissConfig, type DismissOptions, type DismissReason, type OutsidePress } from "./dismiss.js";
export {
  createExitTransition,
  useExitTransition,
  type ExitPhase,
  type ExitState,
  type ExitTransition,
  type ExitTransitionOptions,
} from "./exit.js";
export {
  clampIndex,
  moveIndex,
  readIndex,
  selectByPointer,
  typeaheadMatch,
  useListNavigation,
  type ListMove,
  type ListNavState,
  type ListNavigation,
  type ListNavigationOptions,
} from "./list.js";
export { matchScore, rankCommands, rankItems, type Rankable } from "./rank.js";
