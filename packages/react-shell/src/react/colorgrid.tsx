import { useRef, type KeyboardEvent as ReactKeyboardEvent, type MouseEvent as ReactMouseEvent } from "react";
import { cx } from "./ui.js";
import type { SlotClasses } from "./slots.js";

/** One swatch: a CSS colour value, optionally with a spoken name. */
export type ColorSwatch = string | { value: string; label?: string };

export interface ColorGridProps {
  /** Swatches in reading order. */
  colors: readonly ColorSwatch[];
  /** Selected colour value (compared case-insensitively), or null for none. */
  value?: string | null;
  onChange: (value: string) => void;
  /** Accessible name of the grid. */
  label: string;
  /** Swatches per row. Default 6. */
  columns?: number;
  /**
   * Keep the document selection: swatches do not take focus on press (mousedown default prevented), so an editor
   * keeps its caret and selection while a colour is applied. Default true.
   */
  focusSafe?: boolean;
  className?: string;
  /** Extra classes per region: `root`, `swatch`. */
  slots?: SlotClasses<"root" | "swatch">;
}

const norm = (v: string) => v.trim().toLowerCase();

/**
 * Grid of colour swatches as a single-select `radiogroup`: one tab stop (roving tabindex), arrows move by cell
 * (Left/Right by one, Up/Down by a row), Home/End jump to the ends, Enter or Space picks. Swatch fill is the inline
 * `background` set from the value; border, size and the selected ring belong to the app's CSS on `rk-colorgrid__swatch`
 * and `is-selected`.
 */
export function ColorGrid({ colors, value, onChange, label, columns = 6, focusSafe = true, className, slots }: ColorGridProps) {
  const ref = useRef<HTMLDivElement>(null);
  const items = colors.map((c) => (typeof c === "string" ? { value: c, label: c } : { value: c.value, label: c.label ?? c.value }));
  const cols = Math.max(1, Math.floor(columns));
  const selected = value ? items.findIndex((c) => norm(c.value) === norm(value)) : -1;
  const tabbable = selected >= 0 ? selected : 0;

  const focusAt = (i: number) => ref.current?.querySelectorAll<HTMLElement>('[role="radio"]')[i]?.focus();

  const onKeyDown = (event: ReactKeyboardEvent<HTMLButtonElement>, i: number) => {
    if (event.nativeEvent.isComposing || event.ctrlKey || event.metaKey || event.altKey) return;
    const last = items.length - 1;
    let next: number;
    if (event.key === "ArrowRight") next = Math.min(last, i + 1);
    else if (event.key === "ArrowLeft") next = Math.max(0, i - 1);
    else if (event.key === "ArrowDown") next = i + cols <= last ? i + cols : i;
    else if (event.key === "ArrowUp") next = i - cols >= 0 ? i - cols : i;
    else if (event.key === "Home") next = 0;
    else if (event.key === "End") next = last;
    else return;
    event.preventDefault();
    if (next !== i) focusAt(next);
  };

  const keepSelection = (event: ReactMouseEvent) => {
    if (focusSafe) event.preventDefault();
  };

  return (
    <div
      ref={ref}
      role="radiogroup"
      aria-label={label}
      className={cx("rk-colorgrid", className, slots?.root)}
      style={{ display: "grid", gridTemplateColumns: `repeat(${cols}, max-content)` }}
    >
      {items.map((c, i) => {
        const on = i === selected;
        return (
          <button
            key={`${c.value}:${i}`}
            type="button"
            role="radio"
            aria-checked={on}
            aria-label={c.label}
            title={c.label}
            tabIndex={i === tabbable ? 0 : -1}
            className={cx("rk-colorgrid__swatch", on && "is-selected", slots?.swatch)}
            style={{ background: c.value }}
            onMouseDown={keepSelection}
            onClick={() => onChange(c.value)}
            onKeyDown={(e) => onKeyDown(e, i)}
          />
        );
      })}
    </div>
  );
}
