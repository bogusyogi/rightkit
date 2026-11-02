import { createShortcutRegistry, type ShortcutDef, type ShortcutRegistry } from "@rightkit/react-shell";
import { toRightKitChord } from "./bindings.js";
import { getEffectiveShortcutCombo, getShortcutAction, SHORTCUT_SCHEMA } from "./schema.js";

/** Matching, sequences, suspend/resume, unassigned bindings and conflicts are RightKit's. */
export const rightKitShortcuts: ShortcutRegistry = createShortcutRegistry({ sequenceTimeout: 3000 });

export type HostShortcutDef = Omit<ShortcutDef, "chord" | "group"> & {
  /** Schema id (default chord comes from the shortcut schema) or a fixed shell id with an explicit `chord`. */
  chord?: string;
  group?: string;
  /** Run on auto-repeat key events (a held key). Default false: a repeat is ignored (not run, not prevented). */
  repeat?: boolean;
};

/**
 * Registers one binding. the host app bindings fire while typing (no typing-target guard), so `allowInInput`
 * defaults to true. Returns the unregister function.
 */
export function registerShortcut(def: HostShortcutDef, registry: ShortcutRegistry = rightKitShortcuts): () => void {
  const { repeat = false, run, ...rest } = def;
  const action = getShortcutAction(def.id);
  const schemaChord = action ? toRightKitChord(action.defaultBinding) : null;
  return registry.register({
    allowInInput: true,
    // The shell's own registry also sees every key; a key it already claimed (the palette's `mod+k`) must
    // still reach the host app sequences that start with it.
    allowDefaultPrevented: true,
    rebindable: action?.customizable === true,
    group: "General",
    ...rest,
    // The registry sees every key event, repeats included; each binding decides for itself. A repeat of a
    // binding that does not opt in is ignored: its action does not run and the registry reports the key as
    // not handled, so nothing is default-prevented (as before the registry, when held keys were skipped).
    // It is never reported handled for a key the binding itself might have refused (a `run` that returns
    // false to leave the key to the browser).
    run: (event) => (event.repeat && !repeat ? false : run(event)),
    chord: def.chord ?? (schemaChord ? [schemaChord] : []),
  });
}

/** Pushes the user's stored overrides (the host app combo strings) into the registry; cheap when nothing changed. */
export function syncShortcutBindings(
  overrides: Record<string, string> | undefined,
  registry: ShortcutRegistry = rightKitShortcuts,
): void {
  const next: Record<string, string | null> = {};
  for (const action of SHORTCUT_SCHEMA) {
    if ("prefixStyle" in action && action.prefixStyle) continue;
    const effective = toRightKitChord(getEffectiveShortcutCombo(action.id, overrides));
    if (effective !== toRightKitChord(action.defaultBinding)) next[action.id] = effective;
  }
  if (JSON.stringify(next) !== JSON.stringify(registry.overrides())) registry.setOverrides(next);
}

/** Drops a half-typed sequence (window blur, unmount) without disturbing a modal suspension. */
export function cancelPendingShortcut(registry: ShortcutRegistry = rightKitShortcuts): void {
  if (registry.isSuspended()) return;
  registry.suspend();
  registry.resume();
}
