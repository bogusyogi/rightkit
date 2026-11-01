// Single entry point for OS detection in the renderer. Delegates to the shared
// @rightkit/platform-ui getPlatform(): the native platform once loaded, else the
// host-injected or browser report. The shared "unknown" result (Android, unrecognised
// user agents) maps to "linux", as this module always did. One React bundle ships in
// both desktop installers and the iOS container; this is how the UI adapts at runtime.
// Compile-time native behaviour uses Rust `#[cfg(target_os)]` instead.
import { getPlatform as getSharedPlatform } from "@rightkit/platform-ui";

export type OS = "macos" | "ios" | "windows" | "linux";

export function getPlatform(): OS {
  const p = getSharedPlatform();
  return p === "unknown" ? "linux" : p;
}

export function usePlatform(): OS {
  return getPlatform();
}

export const isMac = () => getPlatform() === "macos";
