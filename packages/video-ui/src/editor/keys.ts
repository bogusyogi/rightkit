import { useEffect, useRef, type RefObject } from "react";
import type { ShortcutDef } from "@rightkit/react-shell";

/**
 * Editor key bindings. The chords and labels are authored once here and registered on the
 * shell's shortcut registry (by DesktopWorkspace), so Settings and the shortcut help list the
 * real bindings. The timeline that is on screen supplies the actions through
 * `useEditorKeys`; with none mounted, or focus outside it, a press is left to the page.
 */
export type EditorKeyId =
  | "undo"
  | "redo"
  | "redoY"
  | "stepBack"
  | "stepForward"
  | "shuttleBack"
  | "pause"
  | "shuttleForward"
  | "remove"
  | "split";

/** An action returns `false` when it did not handle the key, so the page default still runs. */
export type EditorKeyHandlers = Partial<Record<EditorKeyId, (event: KeyboardEvent) => void | boolean>>;

const BINDINGS: { id: EditorKeyId; chord: string | string[]; label: string; hidden?: boolean }[] = [
  { id: "undo", chord: "mod+z", label: "Undo" },
  { id: "redo", chord: "shift+mod+z", label: "Redo" },
  { id: "redoY", chord: "mod+y", label: "Redo", hidden: true },
  { id: "stepBack", chord: "left", label: "Step back one frame" },
  { id: "stepForward", chord: "right", label: "Step forward one frame" },
  { id: "shuttleBack", chord: "j", label: "Shuttle back" },
  { id: "pause", chord: "k", label: "Stop shuttle" },
  { id: "shuttleForward", chord: "l", label: "Shuttle forward" },
  { id: "remove", chord: ["backspace", "delete"], label: "Ripple-delete selection" },
  { id: "split", chord: "s", label: "Split at playhead" },
];

interface ActiveEditor {
  handlers: () => EditorKeyHandlers;
  inScope: () => boolean;
}

let active: ActiveEditor | null = null;

/** The registry definitions for the editor keys (register once, at the workspace). */
export function editorShortcuts(): ShortcutDef[] {
  return BINDINGS.map(({ id, chord, label, hidden }) => ({
    id: `cr.editor.${id}`,
    chord,
    label,
    group: "Editor",
    hidden,
    when: () => active !== null && active.inScope() && active.handlers()[id] !== undefined,
    run: (event) => active?.handlers()[id]?.(event),
  }));
}

/** Makes this editor the one the keys act on while focus is inside `scope`. */
export function useEditorKeys(scope: RefObject<HTMLElement | null>, handlers: EditorKeyHandlers) {
  const latest = useRef(handlers);
  latest.current = handlers;
  useEffect(() => {
    const entry: ActiveEditor = {
      handlers: () => latest.current,
      inScope: () => scope.current?.contains(document.activeElement) ?? false,
    };
    active = entry;
    return () => {
      if (active === entry) active = null;
    };
  }, [scope]);
}
