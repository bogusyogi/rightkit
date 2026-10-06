import { useEffect, useId, useMemo, useRef, useState } from "react";
import { Kbd } from "@rightkit/platform-ui/react";
import type { Command } from "../commands.js";
import { groupByName, initialPaletteSelection, rankItems, selectByKeyboard, selectByPointer, type PaletteSelection } from "../match.js";
import type { ShortcutRegistry } from "../shortcuts.js";
import { chordSteps } from "../shortcuts.js";
import { useFocusTrap, useModalDismiss } from "./hooks.js";

export interface PaletteLabels {
  placeholder: string;
  title: string;
  empty: string;
  results: string;
}

export const DEFAULT_PALETTE_LABELS: PaletteLabels = {
  placeholder: "Search or run a command…",
  title: "Command palette",
  empty: "No matches.",
  results: "Results",
};

export interface CommandPaletteProps {
  commands: readonly Command[];
  onClose: () => void;
  /** Resolves `command.shortcut` ids to live chords. */
  registry?: ShortcutRegistry;
  labels?: Partial<PaletteLabels>;
  limit?: number;
}

/**
 * Cmd/Ctrl+K quick-open + command runner. Ranking merges substring/keyword/subsequence matching;
 * results are grouped by `command.group`; keyboard selection is not stolen by a resting mouse.
 */
export function CommandPalette({ commands, onClose, registry, labels: labelOverrides, limit = 50 }: CommandPaletteProps) {
  const labels = { ...DEFAULT_PALETTE_LABELS, ...labelOverrides };
  const [query, setQuery] = useState("");
  const [selection, setSelection] = useState<PaletteSelection>(initialPaletteSelection);
  const { closing, close } = useModalDismiss(onClose, { escape: false });
  const dialogRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  const id = useId();
  useFocusTrap(dialogRef, true, { autoFocus: false });
  useEffect(() => inputRef.current?.focus(), []);

  const ordered = useMemo(() => {
    const ranked = rankItems(commands, query, limit);
    return groupByName(ranked).flatMap((g) => g.items);
  }, [commands, query, limit]);
  const groups = useMemo(() => groupByName(ordered), [ordered]);
  const sel = Math.min(selection.active, Math.max(0, ordered.length - 1));

  useEffect(() => {
    document.getElementById(`${id}-opt-${sel}`)?.scrollIntoView?.({ block: "nearest" });
  }, [sel, id]);

  const run = (command?: Command) => {
    if (!command) return;
    close();
    // Run after the exit animation so a command that opens another modal keeps the focus it asks for.
    window.setTimeout(() => command.run(), 90);
  };

  const chordOf = (command: Command): string | undefined => (command.shortcut ? registry?.chordFor(command.shortcut) : undefined) ?? command.chord;

  let row = 0;
  return (
    <div className={`rk-overlay rk-overlay--top${closing ? " is-closing" : ""}`} onMouseDown={(e) => e.target === e.currentTarget && close()}>
      <div
        ref={dialogRef}
        role="dialog"
        aria-modal="true"
        aria-label={labels.title}
        className={`rk-palette${closing ? " is-closing" : ""}`}
        onKeyDown={(e) => {
          if (e.key === "Escape") {
            e.preventDefault();
            e.stopPropagation();
            close();
          }
        }}
      >
        <div className="rk-palette__field">
          <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" aria-hidden="true"><circle cx="7" cy="7" r="4.5" /><path d="M10.5 10.5 14 14" /></svg>
          <input
            ref={inputRef}
            role="combobox"
            aria-label={labels.placeholder}
            aria-expanded="true"
            aria-controls={`${id}-list`}
            aria-activedescendant={ordered.length ? `${id}-opt-${sel}` : undefined}
            placeholder={labels.placeholder}
            value={query}
            onChange={(e) => {
              setQuery(e.target.value);
              setSelection(initialPaletteSelection);
            }}
            onKeyDown={(e) => {
              if (e.key === "ArrowDown" || e.key === "ArrowUp") {
                e.preventDefault();
                setSelection(selectByKeyboard({ ...selection, active: sel }, e.key === "ArrowDown" ? "down" : "up", ordered.length));
              } else if (e.key === "Enter") {
                e.preventDefault();
                run(ordered[sel]);
              }
            }}
          />
        </div>
        <div id={`${id}-list`} role="listbox" aria-label={labels.results} className="rk-palette__list">
          {ordered.length === 0 ? <div className="rk-palette__empty">{labels.empty}</div> : null}
          {groups.map((group) => (
            <div key={group.name} role="group" aria-label={group.name}>
              <div className="rk-palette__group" aria-hidden="true">{group.name}</div>
              {group.items.map((command) => {
                const i = row++;
                const chord = chordOf(command);
                return (
                  <div
                    key={command.id}
                    id={`${id}-opt-${i}`}
                    role="option"
                    aria-selected={i === sel}
                    data-command={command.id}
                    className={`rk-palette__opt${i === sel ? " is-active" : ""}`}
                    onMouseMove={(e) => setSelection((cur) => selectByPointer({ ...cur, active: cur.active }, i, { x: e.clientX, y: e.clientY }))}
                    onClick={() => run(command)}
                  >
                    {command.icon ? <span className="rk-palette__icon" aria-hidden="true">{command.icon}</span> : null}
                    <span className="rk-palette__label">{command.label}</span>
                    {chord ? <Kbd chord={chordSteps(chord)[0]} gap={1} platform={registry?.platform} /> : command.hint ? <span className="rk-palette__hint">{command.hint}</span> : null}
                  </div>
                );
              })}
            </div>
          ))}
        </div>
      </div>
    </div>
  );
}

export interface ShortcutHelpProps {
  registry: ShortcutRegistry;
  onClose: () => void;
  title?: string;
}

/** Discoverable-shortcut overlay rendered straight from the registry, so it cannot drift. */
export function ShortcutHelp({ registry, onClose, title = "Keyboard shortcuts" }: ShortcutHelpProps) {
  const groups = registry.groups();
  const titleId = useId();
  const ref = useRef<HTMLDivElement>(null);
  const { closing, close } = useModalDismiss(onClose, { escape: true });
  useFocusTrap(ref, true);
  return (
    <div className={`rk-overlay rk-overlay--center${closing ? " is-closing" : ""}`} onMouseDown={(e) => e.target === e.currentTarget && close()}>
      <div ref={ref} role="dialog" aria-modal="true" aria-labelledby={titleId} className={`rk-modal rk-help${closing ? " is-closing" : ""}`}>
        <div className="rk-help__head">
          <h2 id={titleId} className="rk-modal__title">{title}</h2>
          <button type="button" className="rk-icon-btn" aria-label="Close" title="Close" onClick={close}>✕</button>
        </div>
        <div className="rk-help__grid">
          {groups.map((group) => (
            <section key={group.title}>
              <h3>{group.title}</h3>
              <ul>
                {group.items.map((item) => (
                  <li key={item.id} data-shortcut={item.id}>
                    <span>{item.label}</span>
                    <span className="rk-help__keys">
                      {chordSteps(item.chords[0]).map((step, i) => (
                        <span key={i} className="rk-help__step">
                          {i > 0 ? <em>then</em> : null}
                          <Kbd chord={step} gap={1} platform={registry.platform} />
                        </span>
                      ))}
                    </span>
                  </li>
                ))}
              </ul>
            </section>
          ))}
        </div>
      </div>
    </div>
  );
}
