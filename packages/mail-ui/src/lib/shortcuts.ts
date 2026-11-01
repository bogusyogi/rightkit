// Gmail-style keyboard shortcuts on the shared RightKit registry
// (@rightkit/react-shell). This file owns the binding METADATA (id, default
// chord, label, group, rebindable); the hooks attach `run` handlers to it and
// register the result. Help overlay, Settings and dispatch all read the same
// registry, so they cannot drift. Chord vocabulary: "j", "shift+i", "g i"
// (sequence), "mod+1" (see @rightkit/react-shell shortcuts.ts).
import { createKeymapStore, createShortcutRegistry, type ShortcutRegistry } from "@rightkit/react-shell";

export { isTypingTarget } from "@rightkit/react-shell";

export type ShortcutMeta = {
  /** Stable id: registry key and persisted keymap key (e.g. "trash" -> "#"). */
  id: string;
  /** Default chord(s); the first is the one displayed. */
  chord: string | readonly string[];
  label: string;
  group: string;
  /** Settings offers a Rebind control. */
  rebindable?: boolean;
  /** Dispatches but is not listed in help/Settings. */
  hidden?: boolean;
};

const NAV = "Navigation";
const ACT = "Actions";
const COMP = "Composing";
const OTHER = "Other";

export const SHORTCUT_GROUPS = [NAV, ACT, COMP, OTHER] as const;

export const SHORTCUT_META: ShortcutMeta[] = [
  // Navigation
  { id: "next", chord: "j", label: "Next thread", group: NAV, rebindable: true },
  { id: "prev", chord: "k", label: "Previous thread", group: NAV, rebindable: true },
  { id: "open", chord: ["o", "enter"], label: "Open thread", group: NAV },
  { id: "back", chord: "u", label: "Back to list", group: NAV },
  { id: "backEscape", chord: "esc", label: "Close popover or thread", group: NAV, hidden: true },
  { id: "focusSearch", chord: "/", label: "Focus search", group: NAV },
  { id: "help", chord: "?", label: "Show keyboard shortcuts", group: NAV },
  { id: "findInPage", chord: ["mod+f", "ctrlormeta+f"], label: "Find", group: NAV, hidden: true },
  // Actions
  { id: "archive", chord: "e", label: "Archive", group: ACT, rebindable: true },
  { id: "trash", chord: "#", label: "Delete", group: ACT, rebindable: true },
  { id: "spam", chord: "!", label: "Report spam", group: ACT, rebindable: true },
  { id: "snooze", chord: "b", label: "Snooze", group: ACT, rebindable: true },
  { id: "star", chord: "s", label: "Star / unstar", group: ACT, rebindable: true },
  { id: "label", chord: "l", label: "Label", group: ACT, rebindable: true },
  { id: "markRead", chord: "shift+i", label: "Mark as read", group: ACT },
  { id: "markUnread", chord: "shift+u", label: "Mark as unread", group: ACT },
  { id: "select", chord: "x", label: "Select thread", group: ACT, hidden: true },
  { id: "selectAll", chord: "* a", label: "Select all", group: ACT },
  { id: "selectNone", chord: "* n", label: "Deselect all", group: ACT },
  { id: "undo", chord: "z", label: "Undo last action", group: ACT },
  // Composing
  { id: "compose", chord: "c", label: "Compose", group: COMP, rebindable: true },
  { id: "reply", chord: "r", label: "Reply", group: COMP, rebindable: true },
  { id: "replyAll", chord: "a", label: "Reply all", group: COMP },
  { id: "forward", chord: "f", label: "Forward", group: COMP },
  { id: "composeEscape", chord: "esc", label: "Close compose", group: COMP, hidden: true },
  // Other: go-to sequences
  { id: "goInbox", chord: "g i", label: "Go to Inbox", group: OTHER },
  { id: "goStarred", chord: "g s", label: "Go to Starred", group: OTHER },
  { id: "goSent", chord: "g t", label: "Go to Sent", group: OTHER },
  { id: "goDrafts", chord: "g d", label: "Go to Drafts", group: OTHER },
  { id: "goAllMail", chord: "g a", label: "Go to All Mail", group: OTHER },
  // Workspace app switcher: Ctrl/Cmd+1..6 (APP_OF order in src/store/types.ts).
  { id: "switchToMail", chord: ["mod+1", "ctrlormeta+1"], label: "Switch to Mail", group: OTHER },
  { id: "switchToCalendar", chord: ["mod+2", "ctrlormeta+2"], label: "Switch to Calendar", group: OTHER },
  { id: "switchToDrive", chord: ["mod+3", "ctrlormeta+3"], label: "Switch to Drive", group: OTHER },
  { id: "switchToContacts", chord: ["mod+4", "ctrlormeta+4"], label: "Switch to Contacts", group: OTHER },
  { id: "switchToTasks", chord: ["mod+5", "ctrlormeta+5"], label: "Switch to Tasks", group: OTHER },
  { id: "switchToMeet", chord: ["mod+6", "ctrlormeta+6"], label: "Switch to Meet", group: OTHER },
];

/** Digit -> View for each app-switcher shortcut (the id's `switchTo*` order). */
export const APP_SWITCHER_VIEW_BY_ID: Record<string, "inbox" | "calendar" | "drive" | "contacts" | "tasks" | "meet"> = {
  switchToMail: "inbox",
  switchToCalendar: "calendar",
  switchToDrive: "drive",
  switchToContacts: "contacts",
  switchToTasks: "tasks",
  switchToMeet: "meet",
};

export const SEQUENCE_TIMEOUT_MS = 1200;

const metaById = new Map(SHORTCUT_META.map((m) => [m.id, m]));

/** Metadata + handler for one binding, ready for `registry.register`. */
export function shortcutDef(
  id: string,
  run: (event: KeyboardEvent) => unknown,
  extra: { when?: () => boolean; allowInInput?: boolean } = {},
) {
  const m = metaById.get(id);
  if (!m) throw new Error(`unknown shortcut id: ${id}`);
  return { id, chord: m.chord, label: m.label, group: m.group, rebindable: m.rebindable, hidden: m.hidden, run: (e: KeyboardEvent) => (run(e) === false ? false : undefined), ...extra };
}

// Same storage key and shape the store's `keymap` slice has always used.
const keymapStore = createKeymapStore({ storageKey: "mailright.keymap" });

/** Persisted rebinds as a plain action -> chord map (null/unbound entries dropped). */
export function loadKeymap(): Record<string, string> {
  return Object.fromEntries(Object.entries(keymapStore.load()).filter((e): e is [string, string] => typeof e[1] === "string"));
}

export function saveKeymap(keymap: Record<string, string>): void {
  keymapStore.save(keymap);
}

let registry: ShortcutRegistry | null = null;

/** The one app-wide registry, created on first use (keeps this module DOM-free at import). */
export function getShortcutRegistry(): ShortcutRegistry {
  registry ??= createShortcutRegistry({ sequenceTimeout: SEQUENCE_TIMEOUT_MS });
  return registry;
}

/** Push the user's rebinds into the registry; only rebindable ids are honored. */
export function applyKeymap(keymap: Record<string, string>): void {
  const overrides: Record<string, string> = {};
  for (const [id, chord] of Object.entries(keymap)) {
    if (metaById.get(id)?.rebindable && chord) overrides[id] = chord;
  }
  getShortcutRegistry().setOverrides(overrides);
}
