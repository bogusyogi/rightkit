/**
 * Keyboard registry: one place that owns chords, labels and dispatch, so the help overlay, the
 * palette hints and Settings rebinding can never drift from the real bindings (MailRight's
 * `shortcutRegistry` idea, generalised and merged with ViewRight's typing-target guard).
 *
 * Chord vocabulary (authored once, rendered per OS by `@rightkit/platform-ui`):
 *   "mod+k"        mod = Cmd on macOS, Ctrl elsewhere
 *   "mod+shift+p"  modifiers: mod ctrl meta alt shift
 *   "ctrlormeta+k" exactly one of Ctrl or Meta (Cmd/Win) on every platform, whichever the OS uses
 *   "?"            bare key (never fires while typing in a field)
 *   "g i"          sequence: press g, then i within the timeout (never fires while typing)
 *   "mod+digit"    pseudo-keys: "digit" matches 0-9 and "arrow" matches the four arrow keys (read event.key in `run`)
 */
import { getPlatform, type Platform } from "@rightkit/platform-ui";
import type { ThemeStorage } from "./theme.js";

export interface ShortcutDef {
  /** Stable id: palette hints, rebinding and persistence key off it. */
  id: string;
  chord: string | readonly string[];
  label: string;
  /** Help-overlay section. */
  group?: string;
  /** Return `false` to report "not handled" so the browser default and lower bindings still run. */
  run: (event: KeyboardEvent) => void | boolean;
  /** Evaluated per key press; false skips this binding. */
  when?: () => boolean;
  /** Fire even while focus is in an input. Default: true only for chords with mod/ctrl/meta/alt. */
  allowInInput?: boolean;
  /** Offer in a Settings rebinding UI. */
  rebindable?: boolean;
  /** Omit from help (still dispatches). */
  hidden?: boolean;
  /** Default true. */
  preventDefault?: boolean;
  /** Fire when the event is already `defaultPrevented` (another handler claimed it). Default false. */
  allowDefaultPrevented?: boolean;
  /** Fire during IME composition (`isComposing`). Default false. */
  allowComposing?: boolean;
  /**
   * Match when modifiers the chord does not name are also held (AltGr reports Ctrl+Alt; `esc` with any
   * modifier). Named modifiers must still be held. Default false: modifiers match exactly.
   */
  ignoreExtraModifiers?: boolean;
}

export interface ShortcutInfo {
  id: string;
  label: string;
  group: string;
  /** Effective chords (after overrides). */
  chords: string[];
  defaults: string[];
  rebindable: boolean;
  hidden: boolean;
  overridden: boolean;
  /** Explicitly unbound by an override (`null`), as opposed to bound to its default. `chords` is empty when true. */
  unassigned: boolean;
}

interface Step {
  mod: boolean;
  ctrl: boolean;
  meta: boolean;
  /** Exactly one of Ctrl or Meta (either, with `ignoreExtraModifiers`). */
  ctrlOrMeta: boolean;
  alt: boolean;
  shift: boolean;
  key: string;
}

interface Snapshot {
  key: string;
  code: string;
  ctrl: boolean;
  meta: boolean;
  alt: boolean;
  shift: boolean;
}

const KEY_ALIASES: Record<string, string> = { esc: "escape", plus: "+", return: "enter", del: "delete", up: "arrowup", down: "arrowdown", left: "arrowleft", right: "arrowright" };
const ARROW_KEYS = new Set(["arrowup", "arrowdown", "arrowleft", "arrowright"]);

/** Base character of punctuation keys, by physical `event.code` (used when Alt or AltGr replaces the produced character). */
const CODE_KEYS: Record<string, string> = { Minus: "-", Equal: "=", BracketLeft: "[", BracketRight: "]", Backslash: "\\", Semicolon: ";", Quote: "'", Comma: ",", Period: ".", Slash: "/", Backquote: "`", Space: "space" };

/** The key a physical code names on a US layout, or undefined when the code is not a letter, digit or listed punctuation. */
function keyFromCode(code: string): string | undefined {
  if (/^Key[A-Z]$/.test(code)) return code.slice(3).toLowerCase();
  if (/^Digit[0-9]$/.test(code)) return code.slice(5);
  return CODE_KEYS[code];
}

function parseStep(raw: string): Step {
  const step: Step = { mod: false, ctrl: false, meta: false, ctrlOrMeta: false, alt: false, shift: false, key: "" };
  // "mod++" style: a literal "+" key is written as "plus".
  for (const token of raw.split("+").map((t) => t.trim().toLowerCase()).filter(Boolean)) {
    if (token === "mod") step.mod = true;
    else if (token === "ctrl" || token === "control") step.ctrl = true;
    else if (token === "meta" || token === "cmd" || token === "command") step.meta = true;
    else if (token === "alt" || token === "option") step.alt = true;
    else if (token === "ctrlormeta") step.ctrlOrMeta = true;
    else if (token === "shift") step.shift = true;
    else step.key = KEY_ALIASES[token] ?? token;
  }
  return step;
}

function parseChord(chord: string): Step[] {
  return chord.trim().split(/\s+/).filter(Boolean).map(parseStep);
}

/** Canonical text for conflict detection: sorted modifiers, aliases resolved. */
export function canonicalChord(chord: string): string {
  return parseChord(chord)
    .map((s) => [s.mod && "mod", s.ctrl && "ctrl", s.meta && "meta", s.ctrlOrMeta && "ctrlormeta", s.alt && "alt", s.shift && "shift", s.key].filter(Boolean).join("+"))
    .join(" ");
}

/** Steps of a (possibly sequence) chord, one `Kbd`-renderable string per press. */
export function chordSteps(chord: string): string[] {
  return chord.trim().split(/\s+/).filter(Boolean);
}

function snapshot(event: KeyboardEvent): Snapshot {
  return {
    key: event.key === " " ? "space" : event.key.toLowerCase(),
    code: event.code ?? "",
    ctrl: event.ctrlKey,
    meta: event.metaKey,
    alt: event.altKey,
    shift: event.shiftKey,
  };
}

const MODIFIER_KEYS = new Set(["shift", "control", "alt", "meta", "altgraph", "capslock"]);

/** Exact by default; with `ignore`, a modifier the step requires must be held and any other may be. */
function modifierMatches(wanted: boolean, held: boolean, ignore: boolean): boolean {
  return ignore ? !wanted || held : wanted === held;
}

function matchStep(step: Step, snap: Snapshot, platform: Platform, ignoreExtra = false): boolean {
  const mac = platform === "macos" || platform === "ios";
  const wantMeta = step.meta || (step.mod && mac);
  const wantCtrl = step.ctrl || (step.mod && !mac);
  const primaryOk = step.ctrlOrMeta
    ? ignoreExtra ? snap.ctrl || snap.meta : snap.ctrl !== snap.meta
    : modifierMatches(wantMeta, snap.meta, ignoreExtra) && modifierMatches(wantCtrl, snap.ctrl, ignoreExtra);
  if (!primaryOk || !modifierMatches(step.alt, snap.alt, ignoreExtra)) return false;
  const key = KEY_ALIASES[snap.key] ?? snap.key;
  const physical = keyFromCode(snap.code);
  let keyOk = key === step.key;
  if (!keyOk && step.key === "digit") keyOk = /^[0-9]$/.test(key) || (physical !== undefined && /^[0-9]$/.test(physical));
  if (!keyOk && step.key === "arrow") keyOk = ARROW_KEYS.has(key);
  // Option (macOS) and AltGr rewrite the produced character (Alt+Z -> Omega): fall back to the physical key.
  if (!keyOk && step.alt) keyOk = physical === step.key;
  if (!keyOk) return false;
  // Symbols like "?" or "#" already encode Shift in the produced character.
  const symbol = step.key.length === 1 && !/[a-z0-9]/.test(step.key);
  return symbol || modifierMatches(step.shift, snap.shift, ignoreExtra);
}

/**
 * Chord string for a keydown, in the registry's vocabulary: `mod` for the platform primary (Cmd on macOS, Ctrl
 * elsewhere), then `ctrl`/`meta` for the other one, `alt`, `shift`, and the key. Returns null for a modifier-only press.
 * Alt and AltGr use the physical key so the chord names the key the user pressed, not the character it produced.
 * Shift is omitted for symbol keys, which already encode it. Used by `ShortcutRecorder`.
 */
export function chordFromEvent(event: Pick<KeyboardEvent, "key" | "code" | "ctrlKey" | "metaKey" | "altKey" | "shiftKey">, platform: Platform): string | null {
  const raw = event.key ?? "";
  if (!raw || raw === "Dead" || raw === "Unidentified" || MODIFIER_KEYS.has(raw.toLowerCase())) return null;
  const mac = platform === "macos" || platform === "ios";
  const primary = mac ? event.metaKey : event.ctrlKey;
  const secondary = mac ? event.ctrlKey : event.metaKey;
  let name = raw === " " ? "space" : raw.toLowerCase();
  if (event.altKey) name = keyFromCode(event.code ?? "") ?? name;
  const symbol = name.length === 1 && !/[a-z0-9]/.test(name);
  const parts: string[] = [];
  if (primary) parts.push("mod");
  if (secondary) parts.push(mac ? "ctrl" : "meta");
  if (event.altKey) parts.push("alt");
  if (event.shiftKey && !symbol) parts.push("shift");
  parts.push(name);
  return parts.join("+");
}

/**
 * Held-modifier prefix helpers (CodeRight donor, opt-in): a modifier-only or modifier+key chord
 * (e.g. `"mod"`, `"mod+shift"`) held while a second key is pressed. `heldKeys` holds lower-cased
 * `event.key` names (`"control"`, `"meta"`, `"shift"`, `"alt"`, plus any prefix key). Releasing a
 * modifier removes it from the caller's held set, so the prefix is cancelled automatically.
 * The registry's default dispatch is unchanged; callers opt in by consulting these helpers.
 */
function prefixStep(chord: string): Step | undefined {
  const steps = parseChord(chord);
  return steps.length === 1 ? steps[0] : undefined;
}

export function isShortcutPrefixHeld(
  prefixChord: string,
  heldKeys: ReadonlySet<string>,
  platform: Platform = getPlatform(),
): boolean {
  const step = prefixStep(prefixChord);
  if (!step) return false;
  const mac = platform === "macos" || platform === "ios";
  if (step.mod && !heldKeys.has(mac ? "meta" : "control")) return false;
  if (step.ctrl && !heldKeys.has("control")) return false;
  if (step.meta && !heldKeys.has("meta")) return false;
  if (step.ctrlOrMeta && !heldKeys.has("control") && !heldKeys.has("meta")) return false;
  if (step.alt && !heldKeys.has("alt")) return false;
  if (step.shift && !heldKeys.has("shift")) return false;
  return !step.key || heldKeys.has(step.key.toLowerCase());
}

export function eventMatchesShortcutPrefix(
  event: Pick<KeyboardEvent, "metaKey" | "ctrlKey" | "shiftKey" | "altKey">,
  prefixChord: string,
  heldKeys?: ReadonlySet<string>,
  platform: Platform = getPlatform(),
): boolean {
  const step = prefixStep(prefixChord);
  if (!step) return false;
  const mac = platform === "macos" || platform === "ios";
  const expectedMod = step.mod;
  const modMatches = mac ? event.metaKey : event.ctrlKey;
  if (expectedMod && !modMatches) return false;
  if (!expectedMod && event.metaKey && !step.meta && !step.ctrlOrMeta) return false;
  if (step.shift !== event.shiftKey || step.alt !== event.altKey) return false;
  if (step.ctrl && !event.ctrlKey) return false;
  if (step.meta && !event.metaKey) return false;
  if (step.ctrlOrMeta && event.ctrlKey === event.metaKey) return false;
  if (!step.ctrl && !step.ctrlOrMeta && event.ctrlKey && (!expectedMod || mac)) return false;
  return !step.key || Boolean(heldKeys?.has(step.key.toLowerCase()));
}

/** True when focus is in a text-entry surface that owns bare keys. */
export function isTypingTarget(target: EventTarget | null): boolean {
  const el = target as HTMLElement | null;
  if (!el || typeof el.closest !== "function") return false;
  return el.closest("input:not([type=checkbox]):not([type=radio]):not([type=button]), textarea, select, [contenteditable=''], [contenteditable='true'], .cm-editor") !== null;
}

interface Entry {
  def: ShortcutDef;
  defaults: string[];
}

export interface ShortcutRegistryOptions {
  platform?: Platform;
  /** Milliseconds allowed between steps of a sequence. Default 1000. */
  sequenceTimeout?: number;
}

export function createShortcutRegistry(options: ShortcutRegistryOptions = {}) {
  const platform = options.platform ?? getPlatform();
  const timeout = options.sequenceTimeout ?? 1000;
  const entries = new Map<string, Entry>();
  let overrides: Record<string, string | null> = {};
  let pending: Snapshot[] = [];
  let pendingAt = 0;
  let suspended = false;
  const listeners = new Set<() => void>();
  let version = 0;

  const notify = () => {
    version++;
    for (const l of listeners) l();
  };

  const effective = (entry: Entry): string[] => {
    const o = overrides[entry.def.id];
    if (o === null) return [];
    return o !== undefined ? [o] : entry.defaults;
  };

  function register(def: ShortcutDef): () => void {
    const entry: Entry = { def, defaults: typeof def.chord === "string" ? [def.chord] : [...def.chord] };
    entries.set(def.id, entry);
    notify();
    return () => {
      if (entries.get(def.id) === entry) {
        entries.delete(def.id);
        notify();
      }
    };
  }

  const eligible = (entry: Entry, typing: boolean, chord: string): boolean => {
    if (entry.def.when && entry.def.when() === false) return false;
    if (!typing) return true;
    if (entry.def.allowInInput !== undefined) return entry.def.allowInInput;
    const steps = parseChord(chord);
    if (steps.length !== 1) return false;
    const s = steps[0];
    return s.mod || s.ctrl || s.meta || s.ctrlOrMeta || s.alt;
  };

  /** Per-shortcut opt-ins for composing or already-prevented events. */
  const admits = (entry: Entry, event: KeyboardEvent): boolean =>
    (!event.isComposing || entry.def.allowComposing === true) && (!event.defaultPrevented || entry.def.allowDefaultPrevented === true);

  function handle(event: KeyboardEvent): boolean {
    // Suspended for modal key capture (a shortcut recorder): no binding fires until resume().
    if (suspended) return false;
    // Nothing fires while composing or after another handler prevented the event, unless a shortcut opts in.
    if ((event.isComposing || event.defaultPrevented) && ![...entries.values()].some((entry) => admits(entry, event))) return false;
    const snap = snapshot(event);
    if (MODIFIER_KEYS.has(snap.key)) return false;
    const typing = isTypingTarget(event.target);
    const now = Date.now();
    if (pending.length && now - pendingAt > timeout) pending = [];

    for (let attempt = 0; attempt < 2; attempt++) {
      const seq = [...pending, snap];
      let exact: Entry | undefined;
      let prefix = false;
      for (const entry of entries.values()) {
        for (const chord of effective(entry)) {
          const steps = parseChord(chord);
          if (steps.length < seq.length) continue;
          if (!admits(entry, event)) continue;
          const ignoreExtra = entry.def.ignoreExtraModifiers === true;
          if (!seq.every((s, i) => matchStep(steps[i], s, platform, ignoreExtra))) continue;
          if (!eligible(entry, typing, chord)) continue;
          if (steps.length === seq.length) exact ??= entry;
          else prefix = true;
        }
      }
      if (exact) {
        pending = [];
        const handled = exact.def.run(event);
        if (handled === false) return false;
        if (exact.def.preventDefault !== false) event.preventDefault();
        return true;
      }
      if (prefix) {
        pending = seq;
        pendingAt = now;
        return true;
      }
      if (pending.length === 0) return false;
      pending = []; // abandoned sequence: retry this key on its own
    }
    return false;
  }

  function attach(target: Pick<EventTarget, "addEventListener" | "removeEventListener"> = window): () => void {
    const listener = (e: Event) => void handle(e as KeyboardEvent);
    target.addEventListener("keydown", listener);
    return () => target.removeEventListener("keydown", listener);
  }

  function list(): ShortcutInfo[] {
    return [...entries.values()].map((entry) => ({
      id: entry.def.id,
      label: entry.def.label,
      group: entry.def.group ?? "General",
      chords: effective(entry),
      defaults: entry.defaults,
      rebindable: entry.def.rebindable ?? false,
      hidden: entry.def.hidden ?? false,
      overridden: overrides[entry.def.id] !== undefined,
      unassigned: overrides[entry.def.id] === null,
    }));
  }

  /** Help-overlay grouping: first-seen group order, hidden/unbound entries dropped. */
  function groups(): { title: string; items: ShortcutInfo[] }[] {
    const order: string[] = [];
    const map = new Map<string, ShortcutInfo[]>();
    for (const info of list()) {
      if (info.hidden || info.chords.length === 0) continue;
      if (!map.has(info.group)) {
        map.set(info.group, []);
        order.push(info.group);
      }
      map.get(info.group)!.push(info);
    }
    return order.map((title) => ({ title, items: map.get(title)! }));
  }

  /**
   * Effective bindings that two or more ids share (a Settings rebinding UI should refuse them). `exact`: the same chord.
   * `prefix`: a one-press chord and a sequence that starts with it (`g` and `g i`), so the sequence can never fire.
   * `chord` is the canonical text of the shorter chord.
   */
  function conflicts(): { chord: string; ids: string[]; kind: "exact" | "prefix" }[] {
    const byChord = new Map<string, string[]>();
    for (const info of list()) {
      for (const chord of info.chords) {
        const key = canonicalChord(chord);
        byChord.set(key, [...(byChord.get(key) ?? []), info.id]);
      }
    }
    const exact = [...byChord]
      .filter(([, ids]) => ids.length > 1)
      .map(([chord, ids]) => ({ chord, ids, kind: "exact" as const }));
    const keys = [...byChord.keys()];
    const prefix: { chord: string; ids: string[]; kind: "prefix" }[] = [];
    for (const short of keys) {
      for (const long of keys) {
        if (short === long || !long.startsWith(`${short} `)) continue;
        const ids = [...new Set([...byChord.get(short)!, ...byChord.get(long)!])];
        if (ids.length > 1) prefix.push({ chord: short, ids, kind: "prefix" });
      }
    }
    return [...exact, ...prefix];
  }

  return {
    platform,
    register,
    handle,
    attach,
    list,
    groups,
    conflicts,
    /** Run a binding by id (native menu, palette, tests) without a key event. */
    invoke(id: string): boolean {
      const entry = entries.get(id);
      if (!entry || (entry.def.when && entry.def.when() === false)) return false;
      const event = typeof KeyboardEvent === "function" ? new KeyboardEvent("keydown") : ({} as KeyboardEvent);
      return entry.def.run(event) !== false;
    },
    chordFor: (id: string): string | undefined => {
      const entry = entries.get(id);
      return entry ? effective(entry)[0] : undefined;
    },
    /** `null` unbinds; `undefined` entries restore the default. */
    setOverrides(next: Record<string, string | null>): void {
      overrides = { ...next };
      notify();
    },
    /** Stop dispatching every binding (modal key capture) and drop a half-typed sequence. Keys still reach the page. */
    suspend(): void {
      suspended = true;
      pending = [];
    },
    /** Dispatch bindings again after `suspend()`. */
    resume(): void {
      suspended = false;
    },
    isSuspended: (): boolean => suspended,
    overrides: (): Record<string, string | null> => ({ ...overrides }),
    subscribe(listener: () => void): () => void {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    version: () => version,
  };
}

export type ShortcutRegistry = ReturnType<typeof createShortcutRegistry>;

/** Persist user rebinds. Storage is injectable (browser QA, tests) and failures are silent. */
export function createKeymapStore(options: { storageKey: string; storage?: ThemeStorage | null }) {
  const storage = () => {
    if (options.storage !== undefined) return options.storage;
    try {
      return typeof window === "undefined" ? null : window.localStorage;
    } catch {
      return null;
    }
  };
  return {
    load(): Record<string, string | null> {
      try {
        const raw = storage()?.getItem(options.storageKey);
        const parsed: unknown = raw ? JSON.parse(raw) : {};
        if (parsed && typeof parsed === "object") {
          return Object.fromEntries(Object.entries(parsed).filter(([, v]) => v === null || typeof v === "string")) as Record<string, string | null>;
        }
      } catch {
        /* corrupt or unavailable */
      }
      return {};
    },
    save(overrides: Record<string, string | null>): void {
      try {
        if (Object.keys(overrides).length === 0) storage()?.removeItem(options.storageKey);
        else storage()?.setItem(options.storageKey, JSON.stringify(overrides));
      } catch {
        /* storage unavailable */
      }
    },
  };
}
