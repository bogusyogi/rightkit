export type ReleasePlatform = "darwin" | "windows";

/**
 * The OS the native shell reports, from `window.__RIGHTKIT_PLATFORM__` (injected before the app
 * bundle runs by `lib.rs`). the host app ships for Windows and macOS only; anything else (a browser
 * QA run, an unsupported OS) is `null`.
 */
export function releasePlatform(
  source: { __RIGHTKIT_PLATFORM__?: unknown } = window as { __RIGHTKIT_PLATFORM__?: unknown },
): ReleasePlatform | null {
  const platform = source.__RIGHTKIT_PLATFORM__;
  return platform === "darwin" || platform === "windows" ? platform : null;
}
