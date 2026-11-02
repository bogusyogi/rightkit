// Shortcuts inside an HTML email's iframe document: Find only. The iframe is
// same-origin and focusable, so its keystrokes never reach the app's registry on
// `window`; Ctrl/Cmd+F is attached here (a registry of its own, from the shared
// RightKit implementation) so the app FindBar opens instead of WebView2's native
// find. The app registry is deliberately NOT attached to email content: its bare
// keys (e, #, !, ...) would act on mail while the user reads or selects text.
import { createShortcutRegistry, type ShortcutRegistry } from "@rightkit/react-shell";
import { useAppStore } from "../store/index.js";
import { shortcutDef } from "./shortcuts.js";

/** A registry holding only `findInPage`, with the app's chords for it. */
export function createEmailFrameShortcuts(openFind: () => void, canFind: () => boolean): ShortcutRegistry {
  const registry = createShortcutRegistry();
  registry.register(shortcutDef("findInPage", openFind, { when: canFind, allowInInput: true }));
  return registry;
}

let registry: ShortcutRegistry | null = null;

/** The one registry shared by every email iframe, created on first use. */
export function getEmailFrameShortcuts(): ShortcutRegistry {
  registry ??= createEmailFrameShortcuts(
    () => useAppStore.getState().setFindOpen(true),
    () => !useAppStore.getState().compose,
  );
  return registry;
}
