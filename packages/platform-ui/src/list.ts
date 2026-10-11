/* Headless active-index navigation for a menu, listbox or combobox popup. Returns props; renders nothing. */
import { useEffect, useId, useRef, useState } from "react";

export interface ListNavState {
  /** Raw stored index; the exposed index is this clamped to the item count. */
  index: number;
  /** Last pointer position seen by `"after-pointer-move"`. */
  pointer: { x: number; y: number } | null;
  /** The last selection change came from the keyboard. */
  keyboardDriving: boolean;
}

export type ListMove = "next" | "prev" | "first" | "last";

/** Clamp a stored index to `[0, count - 1]`, or -1 for an empty list. */
export function clampIndex(index: number, count: number): number {
  return count <= 0 ? -1 : Math.min(Math.max(index, 0), count - 1);
}

/**
 * The index to expose for a stored `index`. `"clamp"` (default): into `[0, count - 1]`. `"preserve"`: -1 stays -1
 * ("nothing selected", which ArrowDown on an empty list stores), so results arriving later select nothing; the upper
 * end is still capped at the last item. An empty list is -1 in both modes.
 */
export function readIndex(index: number, count: number, mode: "clamp" | "preserve" = "clamp"): number {
  if (count <= 0) return -1;
  return mode === "preserve" ? Math.max(-1, Math.min(index, count - 1)) : clampIndex(index, count);
}

/**
 * The index a move lands on, from the clamped `current`. `wrap` cycles past the ends, otherwise the move clamps.
 * Disabled items are skipped; when every item is disabled the index is unchanged. An empty list gives -1.
 */
export function moveIndex(current: number, count: number, move: ListMove, wrap: boolean, isDisabled?: (index: number) => boolean, preserveEmpty = false): number {
  if (count <= 0) {
    // Empty list. Default: -1. With `preserveEmpty` (emptyIndex "preserve") the clamp rule is applied to the
    // empty range [0, -1]: next and last cap at the last index (-1), prev and first floor at 0, and a wrapping
    // list has no direction (-1). The result does not depend on `current`.
    return preserveEmpty && !wrap && (move === "prev" || move === "first") ? 0 : -1;
  }
  if (current < 0 && move === "prev") move = wrap ? "last" : "first";
  const ok = (i: number) => !isDisabled?.(i);
  if (move === "first" || move === "last") {
    const order = move === "first" ? Array.from({ length: count }, (_, i) => i) : Array.from({ length: count }, (_, i) => count - 1 - i);
    return order.find(ok) ?? current;
  }
  const step = move === "next" ? 1 : -1;
  let i = current;
  for (let n = 0; n < count; n++) {
    const next = i + step;
    if (wrap) i = (next + count) % count;
    else if (next < 0 || next > count - 1) return current;
    else i = next;
    if (ok(i)) return i;
  }
  return current;
}

/** Pointer policy `"after-pointer-move"`: a resting pointer must not steal the keyboard's selection. */
export function selectByPointer(state: ListNavState, row: number, next: { x: number; y: number }): ListNavState {
  if (state.keyboardDriving) {
    if (state.pointer === null) return { ...state, pointer: next };
    if (state.pointer.x === next.x && state.pointer.y === next.y) return state;
  } else if (state.pointer?.x === next.x && state.pointer.y === next.y) {
    return state;
  }
  return { index: row, pointer: next, keyboardDriving: false };
}

/**
 * Index of the next item whose label starts with `buffer`, searching from the item after `after` and wrapping; -1
 * when none. A longer prefix may stay on the current item; a repeated single character ("ccc") cycles.
 */
export function typeaheadMatch(labels: ReadonlyArray<string>, buffer: string, after: number, isDisabled?: (index: number) => boolean): number {
  const q = buffer.toLowerCase();
  if (!q || !labels.length) return -1;
  const repeated = q.length > 1 && [...q].every((c) => c === q[0]);
  const needle = repeated ? q[0] : q;
  const start = q.length === 1 || repeated ? after + 1 : Math.max(after, 0);
  for (let n = 0; n < labels.length; n++) {
    const i = (start + n) % labels.length;
    if (!isDisabled?.(i) && labels[i].toLowerCase().startsWith(needle)) return i;
  }
  return -1;
}

export interface ListNavigationOptions {
  /** Number of items. */
  count: number;
  /** Index on open and after a reset. Default 0. */
  initialIndex?: number;
  /** ArrowDown past the last item goes to the first, and the reverse; false clamps at the ends. Default true. */
  wrap?: boolean;
  /** Home and End jump to the first and last item. Default true. Turn off for an input whose Home/End move the caret. */
  homeEnd?: boolean;
  /** When `count` changes keep the stored index (clamped on read). Default false: reset to `initialIndex`. */
  keepIndexOnItemsChange?: boolean;
  /** Any change of this value (the query) resets the index to `initialIndex`, even with `keepIndexOnItemsChange`. */
  resetKey?: unknown;
  /**
   * `"always"` (default): every mousemove over an item selects it. `"after-pointer-move"`: a pointer that has not
   * moved never takes the selection back from the keyboard.
   */
  hoverSelects?: "always" | "after-pointer-move";
  /** Typeahead: printable keys on the list select the next item whose label starts with them. Off by default. */
  typeahead?: false | { getLabel: (index: number) => string; resetMs?: number };
  /** Items that navigation, hover and typeahead skip. */
  isDisabled?: (index: number) => boolean;
  /** Called with the active index on an activate key (Enter by default) while an item is active. */
  onActivate?: (index: number) => void;
  /** Keys that activate. Default `["Enter"]`; use `["Enter", " "]` for a menu. */
  activateKeys?: readonly string[];
  /** Prefix for generated ids: `${idPrefix}-list` and `${idPrefix}-option-${i}`. Default: a React `useId`. */
  idPrefix?: string;
  /** Full list id, overriding `idPrefix`. */
  listId?: string;
  /** Full option id, overriding `idPrefix`. */
  optionId?: (index: number) => string;
  /** `"listbox"` (default, rows get role option and aria-selected) or `"menu"` (rows get role menuitem). */
  role?: "listbox" | "menu";
  /** Which element holds focus and so carries aria-activedescendant: the input (default) or the list. */
  focusOwner?: "input" | "list";
  /** Scroll the active option into view (`block: "nearest"`). Default false. */
  scrollIntoView?: boolean;
  /** What triggers that scroll: the active option changing (default), or also the list length changing. */
  scrollOn?: "active" | "active-or-length";
  /** Ignore keys during an IME composition (`isComposing` / `keyCode` 229). Default true. */
  imeGuard?: boolean;
  /**
   * `"clamp"` (default): the exposed index is always inside the list, so ArrowDown on an empty list and then
   * results arriving selects the first item. `"preserve"`: the stored -1 ("nothing selected") survives, nothing is
   * selected when results arrive, and ArrowDown or ArrowUp then select the first item (the last, when wrapping,
   * for ArrowUp). Read the stored value from `rawIndex`. On an empty list `"preserve"` stores what the clamp rule
   * gives (see `moveIndex`): ArrowDown and End store -1, ArrowUp and Home store 0, so results arriving afterwards
   * select the first row after ArrowUp and nothing after ArrowDown. A wrapping list stores -1 for every key.
   */
  emptyIndex?: "clamp" | "preserve";
}

type KeyLike = { key: string; ctrlKey?: boolean; metaKey?: boolean; altKey?: boolean; keyCode?: number; nativeEvent?: { isComposing?: boolean; keyCode?: number }; preventDefault(): void };
type PointerLike = { clientX: number; clientY: number };

export interface ListNavigation {
  /** Active index as exposed (see `emptyIndex`), -1 when nothing is selected or the list is empty. */
  activeIndex: number;
  /** The stored index, before it is read against the current `count`. */
  rawIndex: number;
  activeId: string | undefined;
  setActiveIndex(index: number): void;
  getInputProps(): { role: "combobox"; "aria-expanded": true; "aria-controls": string; "aria-activedescendant": string | undefined; onKeyDown(event: KeyLike): void };
  getListProps(): { id: string; role: "listbox" | "menu"; "aria-activedescendant": string | undefined; onKeyDown(event: KeyLike): void };
  getOptionProps(index: number): { id: string; role: "option" | "menuitem"; "aria-selected": boolean | undefined; "aria-disabled": true | undefined; onMouseMove(event: PointerLike): void };
}

export function useListNavigation(options: ListNavigationOptions): ListNavigation {
  const {
    count,
    initialIndex = 0,
    wrap = true,
    homeEnd = true,
    keepIndexOnItemsChange = false,
    resetKey,
    hoverSelects = "always",
    typeahead = false,
    isDisabled,
    onActivate,
    activateKeys = ["Enter"],
    idPrefix,
    role = "listbox",
    focusOwner = "input",
    scrollIntoView = false,
    scrollOn = "active",
    imeGuard = true,
    emptyIndex = "clamp",
  } = options;
  const generated = useId();
  const prefix = idPrefix ?? generated;
  const listId = options.listId ?? `${prefix}-list`;
  const optionId = options.optionId ?? ((i: number) => `${prefix}-option-${i}`);

  const fresh = (): ListNavState => ({ index: initialIndex, pointer: null, keyboardDriving: false });
  const [state, setState] = useState<ListNavState>(fresh);
  const [seen, setSeen] = useState({ count, resetKey });
  if (seen.count !== count || !Object.is(seen.resetKey, resetKey)) {
    // Adjust state while rendering (React's derived-state pattern): no stale frame with the old index.
    setSeen({ count, resetKey });
    if (!Object.is(seen.resetKey, resetKey) || !keepIndexOnItemsChange) setState(fresh());
  }

  const active = readIndex(state.index, count, emptyIndex);
  const activeId = active >= 0 ? optionId(active) : undefined;

  const latest = useRef({ options, active, count });
  latest.current = { options, active, count };
  const buffer = useRef({ text: "", at: 0 });

  useEffect(() => {
    if (!scrollIntoView || !activeId) return;
    document.getElementById(activeId)?.scrollIntoView?.({ block: "nearest" });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [scrollIntoView, activeId, scrollOn === "active-or-length" ? count : 0]);

  const go = (move: ListMove) => {
    const { active: current, count: n } = latest.current;
    const next = moveIndex(current, n, move, wrap, isDisabled, emptyIndex === "preserve");
    setState((s) => ({ ...s, index: next, keyboardDriving: true }));
  };

  const onKeyDown = (e: KeyLike, onList: boolean) => {
    if (imeGuard && (e.nativeEvent?.isComposing || e.nativeEvent?.keyCode === 229 || e.keyCode === 229)) return;
    const { active: current, count: n } = latest.current;
    if (e.key === "ArrowDown") {
      e.preventDefault();
      go("next");
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      go("prev");
    } else if (homeEnd && e.key === "Home") {
      e.preventDefault();
      go("first");
    } else if (homeEnd && e.key === "End") {
      e.preventDefault();
      go("last");
    } else if (activateKeys.includes(e.key)) {
      e.preventDefault();
      if (current >= 0) onActivate?.(current);
    } else if (onList && typeahead && e.key.length === 1 && !e.ctrlKey && !e.metaKey && !e.altKey && n > 0) {
      const now = Date.now();
      const reset = typeahead.resetMs ?? 500;
      const text = (now - buffer.current.at > reset ? "" : buffer.current.text) + e.key;
      buffer.current = { text, at: now };
      const labels = Array.from({ length: n }, (_, i) => typeahead.getLabel(i));
      const hit = typeaheadMatch(labels, text, current, isDisabled);
      if (hit >= 0) {
        e.preventDefault();
        setState((s) => ({ ...s, index: hit, keyboardDriving: true }));
      }
    }
  };

  return {
    activeIndex: active,
    rawIndex: state.index,
    activeId,
    setActiveIndex: (index) => setState((s) => ({ ...s, index: emptyIndex === "preserve" ? Math.max(-1, index) : clampIndex(index, count) })),
    getInputProps: () => ({
      role: "combobox",
      "aria-expanded": true,
      "aria-controls": listId,
      "aria-activedescendant": focusOwner === "input" ? activeId : undefined,
      onKeyDown: (e) => onKeyDown(e, false),
    }),
    getListProps: () => ({
      id: listId,
      role,
      "aria-activedescendant": focusOwner === "list" ? activeId : undefined,
      onKeyDown: (e) => onKeyDown(e, true),
    }),
    getOptionProps: (index) => ({
      id: optionId(index),
      role: role === "menu" ? "menuitem" : "option",
      "aria-selected": role === "listbox" ? index === active : undefined,
      "aria-disabled": isDisabled?.(index) ? true : undefined,
      onMouseMove: (e) => {
        if (isDisabled?.(index)) return;
        if (hoverSelects === "always") setState((s) => (s.index === index && !s.keyboardDriving ? s : { ...s, index, keyboardDriving: false }));
        else setState((s) => selectByPointer(s, index, { x: e.clientX, y: e.clientY }));
      },
    }),
  };
}
