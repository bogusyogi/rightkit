/**
 * Headless entry: pure helpers and React hooks with no native detection. Nothing here imports
 * `@tauri-apps/plugin-os`, so apps without the plugin (or with no Tauri at all) can use it freely.
 */
export { formatChord, normalizePlatform, type Platform } from "./format.js";
export { floatingPanelStyle, type FloatingPanelInput, type FloatingPanelStyle } from "./geometry.js";
export * from "./toast.js";
export { useFocusTrap, useLatest, useModalDismiss } from "./focus.js";
