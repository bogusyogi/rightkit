import { useEffect, useMemo, useRef } from "react";
import { createShortcutRegistry } from "@rightkit/react-shell";
import { ShortcutHelp as ShellShortcutHelp } from "@rightkit/react-shell/react";

// Single source of truth for the discoverable shortcuts. Mirrors useGlobalShortcuts + the source
// editor keymap so the overlay can't silently drift from the real bindings.
//
// Combos are authored in Windows vocabulary and rendered per-OS (workspace rule: one chord, per-OS
// presentation). The handler itself accepts Ctrl OR Cmd everywhere, so this is purely what we SHOW
// — but showing "Ctrl" to a Mac user is still wrong, because nobody reaches for it there.
// Combos are authored as space-separated Windows vocabulary; convert to a canonical chord and let
// RightKit render it for the current platform.
function comboChord(combo: string): string {
  return combo
    .split(" ")
    .map((key) => (key === "Ctrl" ? "mod" : key.toLowerCase()))
    .join("+");
}

const GROUPS: { title: string; items: [string, string][] }[] = [
  {
    title: "Files & tabs",
    items: [
      ["Ctrl N", "New text document"],
      ["Ctrl T", "New tab"],
      ["Ctrl O", "Open file…"],
      ["Ctrl W", "Close tab"],
      ["Ctrl Shift T", "Reopen closed tab"],
      ["Ctrl Tab", "Next tab"],
      ["Ctrl Shift Tab", "Previous tab"],
      ["Ctrl 1–9", "Jump to tab (9 = last)"],
      ["Ctrl Shift A", "Show all tabs"],
      ["Ctrl E", "Toggle sidebar"],
      ["Ctrl \\", "Toggle sidebar"],
    ],
  },
  {
    title: "Edit & save",
    items: [
      ["Ctrl S", "Save"],
      ["Ctrl Shift S", "Save as…"],
      ["Ctrl P", "Print"],
      ["Ctrl F", "Find"],
      ["F3", "Find next"],
      ["Shift F3", "Find previous"],
      ["Ctrl H", "Replace"],
      ["Alt Z", "Toggle word wrap"],
    ],
  },
  {
    title: "View",
    items: [
      ["Ctrl 0", "Reset font size"],
      ["Ctrl Scroll", "Zoom document"],
      ["Ctrl Shift E", "Toggle Read / Edit (Markup)"],
      ["Ctrl Shift F", "Distraction-free reading"],
      ["Ctrl Shift P", "Command palette"],
      ["?", "This help"],
      ["Esc", "Close find / dialog / help"],
    ],
  },
  {
    title: "Inspect",
    items: [
      // Authored as Ctrl+Alt+I so it renders as ⌘ ⌥ I on macOS — the chord every Mac browser uses.
      // The handler also accepts Ctrl+Shift+I (the Windows/Linux convention). Preview frames own
      // both inspect paths; the host app surrounding shell always consumes them without opening.
      ["Ctrl Alt I", "Open HTML devtools"],
      ["F12", "Open HTML devtools"],
    ],
  },
];

/** A display-only registry: the real bindings live in useGlobalShortcuts, so `run` does nothing. */
function helpRegistry() {
  const registry = createShortcutRegistry();
  for (const group of GROUPS) {
    for (const [combo, label] of group.items) {
      registry.register({ id: `help:${combo}`, label, group: group.title, chord: comboChord(combo), run: () => {} });
    }
  }
  return registry;
}

const SLOTS = {
  root: "vr-pop relative max-h-full w-full max-w-2xl overflow-auto rounded-xl border border-line bg-panel p-5 shadow-2xl",
  title: "font-serif text-lg font-semibold text-paper",
  grid: "grid grid-cols-1 gap-x-8 gap-y-5 sm:grid-cols-2",
  section:
    "[&_h3]:mb-2 [&_h3]:font-sans [&_h3]:text-chrome-xs [&_h3]:uppercase [&_h3]:tracking-[0.2em] [&_h3]:text-muted-2 [&_ul]:space-y-1.5",
  item: "gap-3 text-sm text-paper/85 [&>span:first-child]:min-w-0 [&>span:first-child]:truncate",
  keys: "shrink-0 gap-1 [&_.rightkit-kbd-chord]:gap-1 [&_kbd]:rounded [&_kbd]:border [&_kbd]:border-line [&_kbd]:bg-ink/60 [&_kbd]:px-1.5 [&_kbd]:py-0.5 [&_kbd]:font-mono [&_kbd]:text-chrome-xs [&_kbd]:leading-none [&_kbd]:text-paper",
};

export function ShortcutHelp({ onClose }: { onClose: () => void }) {
  const registry = useMemo(helpRegistry, []);
  const host = useRef<HTMLDivElement>(null);
  // Journeys address this dialog as [role="dialog"][aria-label="Keyboard shortcuts"]; the shell
  // names it with aria-labelledby only, so mirror the same text as aria-label.
  useEffect(() => {
    host.current?.querySelector('[role="dialog"]')?.setAttribute("aria-label", "Keyboard shortcuts");
  }, []);
  return (
    <div ref={host} className="contents">
      <ShellShortcutHelp registry={registry} onClose={onClose} title="Keyboard shortcuts" slots={SLOTS} />
    </div>
  );
}
