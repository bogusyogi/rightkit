import { Fragment, useEffect, useId, useMemo, useRef, useState, type ReactNode } from "react";
import { Kbd } from "@rightkit/platform-ui/react";
import type { Command } from "../commands.js";
import { rankCommands } from "@rightkit/platform-ui/headless";
import { groupByName, initialPaletteSelection, selectByKeyboard, selectByPointer, type PaletteSelection } from "../match.js";
import type { ShortcutInfo, ShortcutRegistry } from "../shortcuts.js";
import { chordSteps } from "../shortcuts.js";
import { useFocusTrap, useModalDismiss } from "./hooks.js";
import { cx } from "./ui.js";
import { useShellOptional } from "./provider.js";
import type { SlotClasses } from "./slots.js";

export interface PaletteLabels {
  placeholder: string;
  title: string;
  empty: string;
  results: string;
  /** Accessible name of the search input. Default: the same string as `placeholder`. */
  inputAriaLabel?: string;
}

export const DEFAULT_PALETTE_LABELS: PaletteLabels = {
  placeholder: "Search or run a command…",
  title: "Command palette",
  empty: "No matches.",
  results: "Results",
};

/** Built-in ranking: substring, keyword, hint and subsequence matching; empty query keeps input order. Lives in `@rightkit/platform-ui/headless`. */
export { rankCommands };

/** Rows moved by PageUp and PageDown. */
const PAGE_STEP = 10;

export interface CommandPaletteProps {
  commands: readonly Command[];
  onClose: () => void;
  /** Resolves `command.shortcut` ids to live chords. */
  registry?: ShortcutRegistry;
  labels?: Partial<PaletteLabels>;
  limit?: number;
  /** Width in px. Default 640. */
  paletteWidth?: number;
  /** Extra class on the palette surface. */
  className?: string;
  /** Extra classes per region: `root`, `field`, `input`, `list`, `group`, `option`, `empty`, `icon`, `scope`, `type`, `sublabel`. */
  slots?: SlotClasses<"root" | "field" | "input" | "list" | "group" | "option" | "empty" | "icon" | "scope" | "type" | "sublabel">;
  /**
   * Replaces the built-in ranking and filtering. Receives all commands, the raw query and `limit`;
   * returns the commands to show, best first. Results are still grouped by `command.group` in
   * first-seen order. Default: `rankCommands`.
   */
  rank?: (commands: readonly Command[], query: string, limit: number) => readonly Command[];
  /** Milliseconds between choosing a command and running it (after the exit animation). Default 90. */
  enterDelayMs?: number;
  /** Replaces the magnifier in the search field. `null` shows none. */
  searchIcon?: ReactNode;
  /**
   * Replaces the content of a command's icon box (`command.icon` by default). Return `null` for no
   * icon box on that row. `active` is true for the highlighted row.
   */
  renderIcon?: (command: Command, state: { active: boolean }) => ReactNode;
  /** Controlled search text. Pass with `onQueryChange` to drive the palette from an external store. */
  query?: string;
  onQueryChange?: (query: string) => void;
  /**
   * Controlled highlight: the index of the highlighted row in the visible order (after grouping). Pass with
   * `onActiveChange`. Without it the palette keeps its own highlight and `onActiveChange` only reports it.
   */
  activeIndex?: number;
  onActiveChange?: (index: number) => void;
  /**
   * Runs the chosen command in place of `command.run()`, after the exit animation. Use it to route every choice through
   * one dispatcher (for example one that also handles scopes or sessions).
   */
  onRun?: (command: Command) => void;
  /** Display order of groups. Groups not listed follow in first-seen order. */
  groupOrder?: readonly string[];
  /**
   * Replaces what a row shows. `defaultRow` is the standard content (icon, label, sub-label, type chip, shortcut or hint).
   * The row element, its role and its id stay the shell's.
   */
  renderRow?: (command: Command, state: { index: number; active: boolean }, defaultRow: ReactNode) => ReactNode;
  /** Node shown as a scope chip in the field, for example "/" for slash commands or "@" for mentions. */
  scopeLabel?: ReactNode;
}

/**
 * Cmd/Ctrl+K quick-open + command runner. Ranking merges substring/keyword/subsequence matching;
 * results are grouped by `command.group`; keyboard selection is not stolen by a resting mouse.
 * Keys: arrows move one row, PageUp/PageDown move ten, Home/End jump to the ends (Home and End move the highlight
 * only while the field is empty; otherwise they move the caret), Enter runs the highlighted row, Escape closes.
 */
export function CommandPalette({
  commands,
  onClose,
  registry,
  labels: labelOverrides,
  limit = 50,
  paletteWidth = 640,
  className,
  slots,
  rank = rankCommands,
  enterDelayMs = 90,
  searchIcon,
  renderIcon,
  query: queryProp,
  onQueryChange,
  activeIndex,
  onActiveChange,
  onRun,
  groupOrder,
  renderRow,
  scopeLabel,
}: CommandPaletteProps) {
  const labels = { ...DEFAULT_PALETTE_LABELS, ...labelOverrides };
  // Without a registry the chord hints are still drawn for the shell's platform.
  const shellPlatform = useShellOptional()?.platform;
  const [innerQuery, setInnerQuery] = useState("");
  const query = queryProp ?? innerQuery;
  const [selection, setSelection] = useState<PaletteSelection>(initialPaletteSelection);
  const { closing, close } = useModalDismiss(onClose, { escape: false });
  const committedRef = useRef(false);
  const dialogRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  const id = useId();
  useFocusTrap(dialogRef, true, { autoFocus: false });
  useEffect(() => inputRef.current?.focus(), []);

  const ordered = useMemo(() => {
    const groups = groupByName(rank(commands, query, limit));
    const order = groupOrder ?? [];
    const weight = (name: string) => {
      const at = order.indexOf(name);
      return at < 0 ? order.length : at;
    };
    const sorted = order.length ? [...groups].sort((a, b) => weight(a.name) - weight(b.name)) : groups;
    return sorted.flatMap((g) => g.items);
  }, [commands, query, limit, rank, groupOrder]);
  const groups = useMemo(() => groupByName(ordered), [ordered]);
  const sel = Math.min(activeIndex ?? selection.active, Math.max(0, ordered.length - 1));

  useEffect(() => {
    document.getElementById(`${id}-opt-${sel}`)?.scrollIntoView?.({ block: "nearest" });
  }, [sel, id]);

  const setQuery = (next: string) => {
    if (queryProp === undefined) setInnerQuery(next);
    onQueryChange?.(next);
  };

  const moveBy = (direction: "down" | "up") => {
    const next = selectByKeyboard({ ...selection, active: sel }, direction, ordered.length);
    if (activeIndex === undefined) setSelection(next);
    onActiveChange?.(next.active);
  };

  const jumpTo = (index: number) => {
    if (!ordered.length) return;
    const next = Math.max(0, Math.min(ordered.length - 1, index));
    if (activeIndex === undefined) setSelection({ ...selection, active: next, keyboardDriving: true });
    onActiveChange?.(next);
  };

  const pointerOver = (row: number, x: number, y: number) => {
    const next = selectByPointer({ ...selection, active: sel }, row, { x, y });
    if (activeIndex === undefined) setSelection(next);
    if (next.active !== sel) onActiveChange?.(next.active);
  };

  const run = (command?: Command) => {
    // Once a command has committed (or the palette is closing), further Enter and click input is ignored
    // until the palette is reopened. The ref is synchronous, so two keydowns in one frame cannot both run.
    if (!command || closing || committedRef.current) return;
    committedRef.current = true;
    close();
    // The timer is deliberately not cleared on unmount: the usual host unmounts the palette from onClose
    // and the chosen command must still run.
    // Run after the exit animation so a command that opens another modal keeps the focus it asks for.
    window.setTimeout(() => (onRun ? onRun(command) : command.run()), enterDelayMs);
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
        style={{ width: paletteWidth }}
        className={cx("rk-palette", closing && "is-closing", className, slots?.root)}
        onKeyDown={(e) => {
          if (e.key === "Escape") {
            e.preventDefault();
            e.stopPropagation();
            close();
          }
        }}
      >
        <div className={cx("rk-palette__field", slots?.field)}>
          {searchIcon !== undefined ? searchIcon : <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" aria-hidden="true"><circle cx="7" cy="7" r="4.5" /><path d="M10.5 10.5 14 14" /></svg>}
          {scopeLabel ? <span className={cx("rk-palette__scope", slots?.scope)}>{scopeLabel}</span> : null}
          <input
            ref={inputRef}
            className={slots?.input}
            role="combobox"
            aria-label={labels.inputAriaLabel ?? labels.placeholder}
            aria-expanded="true"
            aria-controls={`${id}-list`}
            aria-activedescendant={ordered.length ? `${id}-opt-${sel}` : undefined}
            placeholder={labels.placeholder}
            value={query}
            onChange={(e) => {
              setQuery(e.target.value);
              if (activeIndex === undefined) setSelection(initialPaletteSelection);
              onActiveChange?.(0);
            }}
            onKeyDown={(e) => {
              if (closing || committedRef.current) {
                // Exit animation or a command already chosen: no selection moves, no second run.
                e.preventDefault();
                return;
              }
              if (e.key === "ArrowDown" || e.key === "ArrowUp") {
                e.preventDefault();
                moveBy(e.key === "ArrowDown" ? "down" : "up");
              } else if (e.key === "PageDown") {
                e.preventDefault();
                jumpTo(sel + PAGE_STEP);
              } else if (e.key === "PageUp") {
                e.preventDefault();
                jumpTo(sel - PAGE_STEP);
              } else if ((e.key === "Home" || e.key === "End") && query === "") {
                e.preventDefault();
                jumpTo(e.key === "Home" ? 0 : ordered.length - 1);
              } else if (e.key === "Enter") {
                e.preventDefault();
                run(ordered[sel]);
              }
            }}
          />
        </div>
        <div id={`${id}-list`} role="listbox" aria-label={labels.results} className={cx("rk-palette__list", slots?.list)}>
          {ordered.length === 0 ? <div className={cx("rk-palette__empty", slots?.empty)}>{labels.empty}</div> : null}
          {groups.map((group) => (
            <div key={group.name} role="group" aria-label={group.name}>
              <div className={cx("rk-palette__group", slots?.group)} aria-hidden="true">{group.name}</div>
              {group.items.map((command) => {
                const i = row++;
                const chord = chordOf(command);
                const icon = renderIcon ? renderIcon(command, { active: i === sel }) : command.icon;
                const defaultRow = (
                  <>
                    {icon ? <span className={cx("rk-palette__icon", slots?.icon)} aria-hidden="true">{icon}</span> : null}
                    <span className="rk-palette__label">{command.label}</span>
                    {command.sublabel ? <span className={cx("rk-palette__sub", slots?.sublabel)}>{command.sublabel}</span> : null}
                    {command.kind ? <span className={cx("rk-palette__type", slots?.type)}>{command.kind}</span> : null}
                    {chord ? <Kbd chord={chordSteps(chord)[0]} gap={1} platform={registry?.platform ?? shellPlatform} /> : command.hint ? <span className="rk-palette__hint">{command.hint}</span> : null}
                  </>
                );
                return (
                  <div
                    key={command.id}
                    id={`${id}-opt-${i}`}
                    role="option"
                    aria-selected={i === sel}
                    data-command={command.id}
                    className={cx("rk-palette__opt", i === sel && "is-active", slots?.option)}
                    onMouseMove={(e) => pointerOver(i, e.clientX, e.clientY)}
                    onClick={() => run(command)}
                  >
                    {renderRow ? renderRow(command, { index: i, active: i === sel }, defaultRow) : defaultRow}
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
  /** Extra classes per region: `root` (dialog), `title`, `grid`, `section`, `item` (row), `keys` (chord list). */
  slots?: SlotClasses<"root" | "title" | "grid" | "section" | "item" | "keys">;
  /**
   * Replaces one help section. `defaultSection` is the standard markup for the group (heading and list); return it
   * unchanged to keep it, or wrap it. Groups are in the registry's first-seen order.
   */
  renderGroup?: (group: { title: string; items: readonly ShortcutInfo[] }, defaultSection: ReactNode) => ReactNode;
}

/** Discoverable-shortcut overlay rendered straight from the registry, so it cannot drift. */
export function ShortcutHelp({ registry, onClose, title = "Keyboard shortcuts", slots, renderGroup }: ShortcutHelpProps) {
  const groups = registry.groups();
  const titleId = useId();
  const ref = useRef<HTMLDivElement>(null);
  const { closing, close } = useModalDismiss(onClose, { escape: true });
  useFocusTrap(ref, true);
  return (
    <div className={`rk-overlay rk-overlay--center${closing ? " is-closing" : ""}`} onMouseDown={(e) => e.target === e.currentTarget && close()}>
      <div ref={ref} role="dialog" aria-modal="true" aria-labelledby={titleId} className={cx(`rk-modal rk-help${closing ? " is-closing" : ""}`, slots?.root)}>
        <div className="rk-help__head">
          <h2 id={titleId} className={cx("rk-modal__title", slots?.title)}>{title}</h2>
          <button type="button" className="rk-icon-btn" aria-label="Close" title="Close" onClick={close}>✕</button>
        </div>
        <div className={cx("rk-help__grid", slots?.grid)}>
          {groups.map((group) => {
            const section = (
              <section key={group.title} className={slots?.section}>
                <h3>{group.title}</h3>
                <ul>
                  {group.items.map((item) => (
                    <li key={item.id} data-shortcut={item.id} className={slots?.item}>
                      <span>{item.label}</span>
                      <span className={cx("rk-help__keys", slots?.keys)}>
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
            );
            return renderGroup ? <Fragment key={group.title}>{renderGroup({ title: group.title, items: group.items }, section)}</Fragment> : section;
          })}
        </div>
      </div>
    </div>
  );
}
