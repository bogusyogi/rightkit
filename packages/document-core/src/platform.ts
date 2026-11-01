import { formatChord, getPlatform } from "@rightkit/platform-ui";

// Browser QA may select a platform with `?qaPlatform=windows|mac`, only in dev or browser-QA
// builds. @rightkit/platform-ui's `/qa` entry offers the same override, but it must be imported
// statically, which would ship QA code in release bundles; this guard folds away in production.
function qaPlatformOverride(): string {
  if (typeof window === "undefined") return "";
  if (!import.meta.env.DEV && import.meta.env.VITE_RIGHTKIT_BROWSER_QA !== "1") return "";
  return new URLSearchParams(window.location.search).get("qaPlatform")?.toLowerCase() ?? "";
}

// RightKit components (Kbd, ShortcutHelp, palette) call platform-ui's getPlatform(), which honors the
// injected `__RIGHTKIT_PLATFORM__` global when no native platform is loaded. Seed it from the QA
// override so library-rendered chords follow it too; `qaPlatformOverride()` is "" in release builds.
const qaSeed = qaPlatformOverride();
if (qaSeed) {
  globalThis.__RIGHTKIT_PLATFORM__ = qaSeed === "win" ? "windows" : qaSeed === "mac" ? "macos" : qaSeed;
}

/** True when the app is running on macOS (native traffic lights + overlay title bar). */
export function isMac(): boolean {
  const override = qaPlatformOverride();
  if (override) return override === "mac" || override === "macos";
  return getPlatform() === "macos";
}

/** True in Windows desktop builds (borderless window with custom caption buttons). */
export function isWindows(): boolean {
  const override = qaPlatformOverride();
  if (override) return override === "windows" || override === "win";
  return getPlatform() === "windows";
}

/** Render a canonical chord ("mod+shift+s") as "Ctrl+Shift+S" on Windows, "⌘+⇧+S" on macOS. */
export function shortcutLabel(chord: string): string {
  return shortcutTokens(chord).join("+");
}

/** Display tokens for a canonical chord, one per key (for `<kbd>` rendering). */
export function shortcutTokens(chord: string): string[] {
  return formatChord(chord, isMac() ? "macos" : isWindows() ? "windows" : getPlatform(), isMac() ? { labels: { esc: "esc" } } : undefined);
}
