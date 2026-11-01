// Platform detection for UI affordances (keyboard hints, traffic-light spacing).
// Delegates to @rightkit/platform-ui, which reads the native platform through
// @tauri-apps/plugin-os when it is available and otherwise uses the browser's
// own report (correct for every supported desktop webview). Compile-time native
// behaviour uses Rust `#[cfg(target_os)]` instead. Centralised so the Ctrl/⌘
// label and macOS layout tweaks stay consistent.

import { configurePlatform, formatChord, getPlatform, type Platform } from "@rightkit/platform-ui";

export type OS = "macos" | "windows" | "linux";

// The QA platform override lives in the QA-only entry (lib/ipc initIpc), so release builds drop it.
configurePlatform({
  nativeWinsOverQa: true,
  fallbackPlatform: "linux",
});

/** Maps a RightKit platform name onto the three OS families the UI distinguishes. */
export function osFromPlatform(p: Platform): OS {
  if (p === "macos" || p === "ios") return "macos";
  if (p === "windows") return "windows";
  return "linux";
}

/** True when running on macOS. Use for conditional JSX (e.g. TitleBar traffic lights). */
export const isMac = osFromPlatform(getPlatform()) === "macos";

/** Label for the primary command modifier: ⌘ on macOS, Ctrl elsewhere (RightKit's `mod` token). */
export const modKeyLabel: string = formatChord("mod", getPlatform())[0];
