// Storage/display adapter for the host app persisted combo strings ("mod+k t", "mod+comma", "__unassigned__").
// Matching, sequences, suspension and conflicts between registered bindings run in RightKit's
// `createShortcutRegistry` (see ./dispatcher.ts); this file only converts to and from its chord vocabulary.
import { getPlatform } from "@rightkit/platform-ui";
import { formatChord } from "@rightkit/platform-ui/headless";
import { canonicalChord } from "@rightkit/react-shell";

export type ShortcutModifier = "mod" | "shift" | "alt" | "ctrl";
export type ShortcutCombo = string;
export type ShortcutConflict = "exact" | "prefix";

export const UNASSIGNED_SHORTCUT: ShortcutCombo = "__unassigned__";

type ParsedShortcutChord = {
  modifiers: Set<ShortcutModifier>;
  key: string;
};

export type ParsedShortcut = {
  chords: ReadonlyArray<ParsedShortcutChord>;
};

const MODIFIER_KEY_MAP: Record<string, ShortcutModifier> = {
  mod: "mod",
  shift: "shift",
  alt: "alt",
  option: "alt",
  ctrl: "ctrl",
  control: "ctrl",
  meta: "mod",
  cmd: "mod",
  command: "mod",
};

const KEY_LABEL_MAP: Record<string, string> = {
  comma: ",",
  period: ".",
  enter: "Enter",
  escape: "Esc",
  tab: "Tab",
  space: "Space",
  backspace: "⌫",
  delete: "⌦",
  arrowup: "↑",
  arrowdown: "↓",
  arrowleft: "←",
  arrowright: "→",
  home: "Home",
  end: "End",
  pageup: "Page Up",
  pagedown: "Page Down",
};

const MODIFIER_PRIORITY: ShortcutModifier[] = ["mod", "ctrl", "shift", "alt"];
const RISKY_BROWSER_SHORTCUT_KEYS = new Set(["w", "t", "r", "p", "s", "f", "l", "n", "q", "d", "h", "j", "o", "u"]);

const SHIFTED_KEY_BASE_MAP: Record<string, string> = {
  "{": "[",
  "}": "]",
  ":": ";",
  '"': "'",
  "<": ",",
  ">": ".",
  "?": "/",
  "|": "\\",
  "~": "`",
  "!": "1",
  "@": "2",
  "#": "3",
  "$": "4",
  "%": "5",
  "^": "6",
  "&": "7",
  "*": "8",
  "(": "9",
  ")": "0",
};

function isUnassignedShortcut(combo: ShortcutCombo): boolean {
  return combo.trim().toLowerCase() === UNASSIGNED_SHORTCUT;
}

export function isMacOS(): boolean {
  const platform = getPlatform();
  return platform === "macos" || platform === "ios";
}

export function keyToShortcutToken(key: string): string {
  const lowered = key.toLowerCase();
  if (lowered === ",") return "comma";
  if (lowered === ".") return "period";
  if (lowered === " ") return "space";
  if (lowered === "esc") return "escape";
  if (lowered === "+") return "plus";
  if (lowered === "-" || lowered === "_") return "minus";
  if (lowered === "arrowup" || lowered === "arrowdown" || lowered === "arrowleft" || lowered === "arrowright") {
    return lowered;
  }
  return SHIFTED_KEY_BASE_MAP[lowered] ?? lowered;
}

export function normalizeCombo(combo: ShortcutCombo): ShortcutCombo {
  if (isUnassignedShortcut(combo)) return UNASSIGNED_SHORTCUT;
  const chords = combo.trim().replace(/\s*\+\s*/g, "+").split(/\s+/).filter(Boolean);
  if (chords.length === 0 || chords.length > 2) return "";
  return chords.map(normalizeChord).join(" ");
}

function normalizeChord(combo: ShortcutCombo): ShortcutCombo {
  const rawParts = combo.toLowerCase().trim().split("+").map((part) => part.trim()).filter(Boolean);
  const modifiers = new Set<ShortcutModifier>();
  let key = "";
  for (const rawPart of rawParts) {
    const part = rawPart === "," ? "comma" : rawPart === "." ? "period" : rawPart;
    const modifier = MODIFIER_KEY_MAP[part];
    if (modifier) modifiers.add(modifier);
    else key = part;
  }
  return [...MODIFIER_PRIORITY.filter((modifier) => modifiers.has(modifier)), key].filter(Boolean).join("+");
}

export function isValidShortcutCombo(combo: ShortcutCombo): boolean {
  if (isUnassignedShortcut(combo)) return true;
  const parsed = parseShortcut(combo);
  return parsed !== undefined && parsed.chords.every((chord) => chord.key.trim().length > 0);
}

export function parseShortcut(combo: ShortcutCombo): ParsedShortcut | undefined {
  if (isUnassignedShortcut(combo)) {
    return { chords: [{ modifiers: new Set<ShortcutModifier>(), key: UNASSIGNED_SHORTCUT }] };
  }
  const normalized = normalizeCombo(combo);
  if (!normalized) return undefined;
  return {
    chords: normalized.split(" ").map((chord) => {
      const modifiers = new Set<ShortcutModifier>();
      let key = "";
      for (const part of chord.split("+")) {
        const modifier = MODIFIER_KEY_MAP[part];
        if (modifier) modifiers.add(modifier);
        else key = part;
      }
      return { modifiers, key };
    }),
  };
}

function getShortcutDisplayPlatform(): "macos" | "other" {
  return isMacOS() ? "macos" : "other";
}

export function formatShortcutForDisplay(
  combo: ShortcutCombo,
  unassignedLabel = "Unassigned",
  platform = getShortcutDisplayPlatform(),
): string {
  if (isUnassignedShortcut(combo)) return unassignedLabel;
  const parsed = parseShortcut(combo);
  if (!parsed || parsed.chords.some((chord) => !chord.key && chord.modifiers.size === 0)) return unassignedLabel;
  return parsed.chords.map((chord) => formatChordForDisplay(chord, platform)).join(", ");
}

function formatChordForDisplay(parsed: ParsedShortcutChord, platform: "macos" | "other"): string {
  // Modifier and key spelling comes from RightKit's `formatChord` (mod/ctrl/shift/alt render as
  // the platform's own symbols or words); `KEY_LABEL_MAP` keeps the host app wording for named keys.
  const tokens: string[] = MODIFIER_PRIORITY.filter((modifier) => parsed.modifiers.has(modifier));
  if (parsed.key) tokens.push(parsed.key.toLowerCase());
  return formatChord(tokens.join("+"), platform === "macos" ? "macos" : "windows", {
    labels: KEY_LABEL_MAP,
  }).join(" + ");
}

export function getShortcutConflict(left: ShortcutCombo, right: ShortcutCombo): ShortcutConflict | undefined {
  const normalizedLeft = normalizeCombo(left);
  const normalizedRight = normalizeCombo(right);
  if (!isValidShortcutCombo(normalizedLeft) || !isValidShortcutCombo(normalizedRight)) return undefined;
  if (normalizedLeft === UNASSIGNED_SHORTCUT || normalizedRight === UNASSIGNED_SHORTCUT) return undefined;
  if (normalizedLeft === normalizedRight) return "exact";
  const leftChord = canonicalChord(toRightKitChord(normalizedLeft) ?? "");
  const rightChord = canonicalChord(toRightKitChord(normalizedRight) ?? "");
  if (leftChord === rightChord) return "exact";
  return rightChord.startsWith(`${leftChord} `) || leftChord.startsWith(`${rightChord} `) ? "prefix" : undefined;
}

const RIGHTKIT_KEY_TOKENS: Record<string, string> = { comma: ",", period: ".", minus: "-" };

/** the host app combo to a RightKit registry chord; null when unassigned or invalid. */
export function toRightKitChord(combo: ShortcutCombo): string | null {
  const normalized = normalizeCombo(combo);
  if (!normalized || normalized === UNASSIGNED_SHORTCUT) return null;
  return normalized
    .split(" ")
    .map((step) => step.split("+").map((part) => RIGHTKIT_KEY_TOKENS[part] ?? part).join("+"))
    .join(" ");
}

export function isRiskyBrowserShortcut(combo: ShortcutCombo): boolean {
  if (isUnassignedShortcut(combo)) return false;
  const parsed = parseShortcut(combo);
  if (!parsed) return false;
  return parsed.chords.some((chord) => {
    if (!chord.modifiers.has("mod") || chord.modifiers.has("alt")) return false;
    if (chord.modifiers.has("shift")) return chord.key.toLowerCase() === "w" || chord.key.toLowerCase() === "q";
    return RISKY_BROWSER_SHORTCUT_KEYS.has(chord.key.toLowerCase());
  });
}

const CODE_KEY_MAP = new Map<string, string>([
  ["Comma", ","], ["Period", "."], ["Slash", "/"], ["Backquote", "`"],
  ["BracketLeft", "["], ["BracketRight", "]"], ["Semicolon", ";"], ["Quote", "'"],
  ["Minus", "-"], ["Equal", "="],
]);

function keyFromEventCode(code: string): string | null {
  if (code.startsWith("Key") && code.length === 4) return code.slice(3).toLowerCase();
  if (code.startsWith("Digit") && code.length === 6) return code.slice(5);
  return CODE_KEY_MAP.get(code) ?? null;
}

export function resolveShortcutEventKey(event: Pick<KeyboardEvent, "key" | "code" | "altKey">): string {
  const raw = event.key;
  if (event.altKey || (raw.length === 1 && raw.charCodeAt(0) > 127)) return keyFromEventCode(event.code) ?? raw;
  return raw;
}
