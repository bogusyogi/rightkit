import type { OS } from "./types.js";

export function chordDisplayKeyForEvent(key: string, os: OS): string | null {
  switch (key.toLowerCase()) {
    case "control":
      return os === "macos" ? "⌃" : "Ctrl";
    case "shift":
      return os === "macos" ? "⇧" : "Shift";
    case "alt":
      return os === "macos" ? "⌥" : "Alt";
    case "meta":
      return os === "macos" ? "⌘" : "Win";
    case " ":
    case "spacebar":
      return "Space";
    case "escape":
      return os === "macos" ? "esc" : "Esc";
    default:
      return null;
  }
}
