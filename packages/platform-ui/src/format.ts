/** Pure platform naming and chord display. Imports nothing native: safe for the headless entry. */
export type Platform = "macos" | "ios" | "windows" | "linux" | "unknown";

export function normalizePlatform(value: string): Platform {
  if (value === "Win32" || value === "win32") return "windows";
  if (value === "macos" || value === "ios" || value === "windows" || value === "linux") {
    return value;
  }
  return "unknown";
}

const MAC_KEYS: Readonly<Record<string, string>> = {
  mod: "⌘",
  ctrl: "⌃",
  alt: "⌥",
  shift: "⇧",
  fn: "fn",
  win: "⌘",
  space: "Space",
  enter: "↩",
  esc: "Esc",
};

const WINDOWS_KEYS: Readonly<Record<string, string>> = {
  mod: "Ctrl",
  ctrl: "Ctrl",
  rctrl: "Right Ctrl",
  alt: "Alt",
  ralt: "Right Alt",
  shift: "Shift",
  fn: "Fn",
  win: "Win",
  space: "Space",
  enter: "Enter",
  esc: "Esc",
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
};

const GENERIC_KEYS: Readonly<Record<string, string>> = {
  mod: "Mod",
  ctrl: "Ctrl",
  rctrl: "Right Ctrl",
  alt: "Alt",
  ralt: "Right Alt",
  shift: "Shift",
  fn: "Fn",
  win: "System",
  space: "Space",
  enter: "Enter",
  esc: "Esc",
};

/** Display tokens for a canonical chord on `platform` (default `unknown`: generic labels). */
export function formatChord(chord: string, platform: Platform = "unknown"): string[] {
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
      keys[token] ??
      (token.length === 1 ? token.toUpperCase() : token.replace(/^\w/, (letter) => letter.toUpperCase())),
    );
}
