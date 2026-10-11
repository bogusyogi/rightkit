/** Pure platform naming and chord display. Imports nothing native: safe for the headless entry. */
export type Platform = "macos" | "ios" | "windows" | "linux" | "unknown";

/**
 * Canonical platform for a raw name. Accepts the Tauri names, the aliases `mac` and `darwin` (macOS),
 * `win` and `win32` / `Win32` (Windows); anything else is `unknown`.
 */
export function normalizePlatform(value: string): Platform {
  if (value === "Win32" || value === "win32" || value === "win") return "windows";
  if (value === "mac" || value === "darwin") return "macos";
  if (value === "macos" || value === "ios" || value === "windows" || value === "linux") {
    return value;
  }
  return "unknown";
}

const MAC_KEYS: Readonly<Record<string, string>> = {
  mod: "⌘",
  ctrl: "⌃",
  lctrl: "Left Control",
  rctrl: "Right Control",
  alt: "⌥",
  lalt: "Left Option",
  ralt: "Right Option",
  shift: "⇧",
  lshift: "Left Shift",
  rshift: "Right Shift",
  fn: "fn",
  win: "⌘",
  space: "Space",
  enter: "↩",
  esc: "Esc",
  ctrlormeta: "⌘/⌃",
};

const WINDOWS_KEYS: Readonly<Record<string, string>> = {
  mod: "Ctrl",
  ctrl: "Ctrl",
  lctrl: "Left Ctrl",
  rctrl: "Right Ctrl",
  alt: "Alt",
  lalt: "Left Alt",
  ralt: "Right Alt",
  shift: "Shift",
  lshift: "Left Shift",
  rshift: "Right Shift",
  fn: "Fn",
  win: "Win",
  space: "Space",
  enter: "Enter",
  esc: "Esc",
  ctrlormeta: "Ctrl/Win",
};

const IOS_KEYS: Readonly<Record<string, string>> = {
  mod: "Touch",
  ctrl: "Touch",
  alt: "Touch",
  shift: "Touch",
  fn: "Touch",
  win: "Touch",
  space: "Record",
  enter: "Send",
  esc: "Cancel",
  record: "Record",
  send: "Send",
  cancel: "Cancel",
  ctrlormeta: "Touch",
};

const GENERIC_KEYS: Readonly<Record<string, string>> = {
  mod: "Mod",
  ctrl: "Ctrl",
  lctrl: "Left Ctrl",
  rctrl: "Right Ctrl",
  alt: "Alt",
  lalt: "Left Alt",
  ralt: "Right Alt",
  shift: "Shift",
  lshift: "Left Shift",
  rshift: "Right Shift",
  fn: "Fn",
  win: "System",
  space: "Space",
  enter: "Enter",
  esc: "Esc",
  ctrlormeta: "Ctrl/Meta",
};

export interface FormatChordOptions {
  /** Labels by canonical token (lower case) that win over the platform's own, e.g. `{ esc: "esc" }`. */
  labels?: Readonly<Record<string, string>>;
}

/**
 * Display tokens for a canonical chord on `platform` (default `unknown`: generic labels). Side-specific
 * modifiers (`lctrl`, `rctrl`, `lalt`, `ralt`, `lshift`, `rshift`) are spelled out on every platform.
 * `ctrlormeta` ("Ctrl or Meta", the shortcut modifier that is Ctrl or Cmd/Win) renders as "⌘/⌃" on macOS,
 * "Ctrl/Win" on Windows and Linux, "Touch" on iOS, "Ctrl/Meta" when the platform is unknown.
 */
export function formatChord(chord: string, platform: Platform = "unknown", options: FormatChordOptions = {}): string[] {
  const keys =
    platform === "macos"
      ? MAC_KEYS
      : platform === "ios"
        ? IOS_KEYS
        : platform === "windows" || platform === "linux"
          ? WINDOWS_KEYS
          : GENERIC_KEYS;
  return chord
    .split("+")
    .map((token) => token.trim().toLowerCase())
    .filter(Boolean)
    .map((token) =>
      options.labels?.[token] ??
      keys[token] ??
      (token.length === 1 ? token.toUpperCase() : token.replace(/^\w/, (letter) => letter.toUpperCase())),
    );
}

/**
 * Source text for a host's initialization script (Tauri `initialization_script`, or any script that runs before
 * the app bundle) so `getPlatform()` is correct on the first synchronous read. The value goes through
 * `normalizePlatform`, so `"mac"` becomes `"macos"` and an unsupported name becomes `"unknown"`; the output
 * contains only a JSON string literal from that closed set. Pure: no DOM, no native import.
 */
export function platformInitializationScript(platform: string): string {
  return `window.__RIGHTKIT_PLATFORM__ = ${JSON.stringify(normalizePlatform(platform))};`;
}
