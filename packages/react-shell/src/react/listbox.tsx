import { useEffect, useId, useRef, type ReactNode } from "react";
import type { ListNavigation } from "@rightkit/platform-ui/headless";
import { Popover, type PopoverProps } from "./popover.js";
import { cx } from "./ui.js";

export interface ListboxProps {
  /** Element the listbox opens from (see `Popover`). */
  anchor: PopoverProps["anchor"];
  /** Called on Escape or an outside press. */
  onClose: () => void;
  /** Accessible name of the listbox. */
  label: string;
  /** The navigation from `useListNavigation`. Create it with `focusOwner: "list"` so the listbox holds focus. */
  nav: ListNavigation;
  placement?: PopoverProps["placement"];
  width?: number;
  className?: string;
  children: ReactNode;
}

/**
 * An anchored listbox: `Popover` with a `role="listbox"` element that takes focus on open and receives the keys from
 * `useListNavigation`. Rows are `ListboxOption`s, optionally in `ListboxGroup`s; the row content stays the app's.
 */
export function Listbox({ anchor, onClose, label, nav, placement, width, className, children }: ListboxProps) {
  const listRef = useRef<HTMLDivElement>(null);

  // Runs after Popover's own focus effect (a parent's effect runs after its children's), so the list keeps focus.
  useEffect(() => {
    listRef.current?.focus({ preventScroll: true });
  }, []);

  return (
    <Popover anchor={anchor} onClose={onClose} label={label} placement={placement} width={width} className={cx("rk-listbox-popover", className)}>
      <div
        ref={listRef}
        {...nav.getListProps()}
        tabIndex={-1}
        aria-label={label}
        className="rk-listbox"
      >
        {children}
      </div>
    </Popover>
  );
}

export interface ListboxGroupProps {
  /** Visible group heading. It names the group for assistive technology. */
  label: ReactNode;
  className?: string;
  children: ReactNode;
}

/** A labelled run of options inside a `Listbox`. */
export function ListboxGroup({ label, className, children }: ListboxGroupProps) {
  const labelId = useId();
  return (
    <div role="group" aria-labelledby={labelId} className={cx("rk-listbox__group", className)}>
      <div id={labelId} className="rk-listbox__group-label">{label}</div>
      {children}
    </div>
  );
}

export interface ListboxOptionProps {
  /** The navigation from `useListNavigation`, the same one passed to `Listbox`. */
  nav: ListNavigation;
  /** This row's index in the navigation's items. */
  index: number;
  /** Called with the index on a click. Keyboard activation goes through `useListNavigation`'s `onActivate`. */
  onSelect?: (index: number) => void;
  className?: string;
  children: ReactNode;
}

/** One row of a `Listbox`: `role="option"` with the active, selected and disabled state from the navigation. */
export function ListboxOption({ nav, index, onSelect, className, children }: ListboxOptionProps) {
  const props = nav.getOptionProps(index);
  const active = nav.activeIndex === index;
  return (
    <div
      {...props}
      className={cx("rk-listbox__option", active && "is-active", props["aria-disabled"] && "is-disabled", className)}
      onClick={() => {
        if (!props["aria-disabled"]) onSelect?.(index);
      }}
    >
      {children}
    </div>
  );
}
