// Renders a keyboard chord with the right symbols for the current OS from ONE
// chord definition - the cross-platform "CmdOrCtrl" pattern. Define the chord
// once (e.g. "mod+space"); this shows ⌘Space on macOS, Ctrl+Space on Windows.
//
// Tokens: mod (⌘/Ctrl) · ctrl · alt (⌥/Alt) · shift (⇧/Shift) · fn · space ·
// win · enter · esc · record · send · cancel · plain letters. The exported chord
// constants keep copy, onboarding, and Settings in sync across platforms.
import { formatChord as formatPlatformChord } from "@rightkit/platform-ui";
import { getPlatform, type OS } from "../hooks/usePlatform";

// the host app shows the macOS Escape key in lower case, as on the keycap.
const MAC_LABELS: Record<string, string> = { esc: "esc" };

export function formatChord(chord: string, os: OS = getPlatform()): string[] {
  return formatPlatformChord(chord, os, os === "macos" ? { labels: MAC_LABELS } : {});
}

const PLATFORM = getPlatform();

// macOS captures fn chords via a CGEventTap. Windows uses Ctrl chords via the
// global-shortcut plugin, including global cancel. iOS is in-app touch controls.
// Everything that renders <Kbd> stays in sync from here.
export const DICTATION_CHORD =
  PLATFORM === "macos" ? "fn+space" : PLATFORM === "ios" ? "record" : "ctrl+space";
export const STOP_AND_SEND_CHORD =
  PLATFORM === "macos" ? "fn+shift+space" : PLATFORM === "ios" ? "send" : "ctrl+shift+space";
export const CANCEL_CHORD =
  PLATFORM === "macos" ? "fn+esc" : PLATFORM === "ios" ? "cancel" : "ctrl+esc";

export function pushToTalkChord(os: OS = PLATFORM, pttKey: string = "ralt"): string {
  if (os === "macos") return pttKey === "lalt" || pttKey === "ralt" ? pttKey : "control";
  if (os === "ios") return "record";
  return pttKey === "rctrl" ? "rctrl" : pttKey === "lalt" ? "lalt" : "ralt";
}

export function pushToTalkCopy(os: OS = PLATFORM, pttKey: string = "ralt"): string {
  if (os === "macos") {
    const key = pttKey === "lalt" ? "Left Option" : pttKey === "ralt" ? "Right Option" : "Control";
    return `Hold ${key}, speak, release to paste. Press fn before releasing to paste + send.`;
  }
  if (os === "ios") return "Use the in-app record control.";
  return pttKey === "rctrl"
    ? "Hold Right Ctrl, speak, release to paste. Add Right Alt to paste + send."
    : pttKey === "lalt"
      ? "Hold Left Alt, speak, release to paste. Add Right Ctrl to paste + send."
      : "Hold Right Alt, speak, release to paste. Add Right Ctrl to paste + send.";
}

export default function Kbd({ chord, gap = 4 }: { chord: string; gap?: number }) {
  const keys = formatChord(chord);
  // The inner <kbd> style is a shared class rather than a per-key inline
  // object. The outer span needs the dynamic `gap` prop, so it keeps an
  // inline style that only sets a CSS variable the class consumes.
  return (
    <span
      className="hr-kbd-chord"
      style={{ ["--hr-kbd-gap" as string]: `${gap}px` } as React.CSSProperties}
    >
      {keys.map((k) => (
        <kbd key={k} className="hr-kbd-key">
          {k}
        </kbd>
      ))}
    </span>
  );
}
