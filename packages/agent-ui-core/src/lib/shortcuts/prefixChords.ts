// Held-modifier prefix chords (a modifier-only chord such as "mod" held while a second key is pressed) are
// @rightkit/react-shell's `isShortcutPrefixHeld` / `eventMatchesShortcutPrefix` (0.7.1, ported from this
// app's donor code; closes bogusyogi/rightkit#655). This file only maps the host app combo vocabulary
// (`normalizeCombo`, the unassigned sentinel, `isMacOS`) onto them.
import {
  eventMatchesShortcutPrefix as shellEventMatchesShortcutPrefix,
  isShortcutPrefixHeld as shellIsShortcutPrefixHeld,
} from "@rightkit/react-shell";
import { isMacOS, normalizeCombo, UNASSIGNED_SHORTCUT, type ShortcutCombo } from "./bindings";

/** The single-chord canonical form of a prefix combo, or undefined for unassigned, invalid or multi-chord combos. */
function prefixChord(prefixCombo: ShortcutCombo): string | undefined {
  const normalized = normalizeCombo(prefixCombo);
  return normalized && normalized !== UNASSIGNED_SHORTCUT && normalized.split(" ").length === 1
    ? normalized
    : undefined;
}

function platform() {
  return isMacOS() ? "macos" : "windows";
}

export function isShortcutPrefixHeld(prefixCombo: ShortcutCombo, heldKeys: ReadonlySet<string>): boolean {
  const chord = prefixChord(prefixCombo);
  return chord ? shellIsShortcutPrefixHeld(chord, heldKeys, platform()) : false;
}

export function eventMatchesShortcutPrefix(
  event: Pick<KeyboardEvent, "metaKey" | "ctrlKey" | "shiftKey" | "altKey">,
  prefixCombo: ShortcutCombo,
  heldKeys?: ReadonlySet<string>,
): boolean {
  const chord = prefixChord(prefixCombo);
  return chord ? shellEventMatchesShortcutPrefix(event, chord, heldKeys, platform()) : false;
}
