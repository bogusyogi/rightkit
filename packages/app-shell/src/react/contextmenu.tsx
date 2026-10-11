import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState, type MouseEvent as ReactMouseEvent, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { Kbd } from "@rightkit/platform-ui/react";
import { floatingPanelStyle } from "../geometry.js";
import { cx } from "./ui.js";
import type { SlotClasses } from "./slots.js";

export interface MenuItem {
  id: string;
  label: ReactNode;
  icon?: ReactNode;
  /** Chord to display, e.g. "mod+c". Display only: the app's own binding does the work. */
  shortcut?: string;
  disabled?: boolean;
  danger?: boolean;
  /** Renders as a checkable row (menuitemcheckbox). */
  checked?: boolean;
  separatorBefore?: boolean;
  run?: () => void;
}

export interface ContextMenuProps {
  x: number;
  y: number;
  items: readonly MenuItem[];
  onClose: () => void;
  /** Accessible name, e.g. "Document actions". */
  label: string;
  width?: number;
  /** Extra class on the menu element. */
  className?: string;
  /** Extra classes per region: `root` (menu), `item`, `separator`, `label`, `lead`, `scrim`. */
  slots?: SlotClasses<"root" | "item" | "separator" | "label" | "lead" | "scrim">;
  /** Extra class on the click-away scrim (same as `slots.scrim`). */
  scrimClassName?: string;
  /**
   * Element used for each row. Default `"div"` (aria-disabled for disabled rows). `"button"` renders
   * `button[type=button][role=menuitem|menuitemcheckbox]` with a native `disabled` attribute; rows
   * stay out of the Tab order because the menu itself holds focus (aria-activedescendant).
   */
  itemAs?: "div" | "button";
  /** Replaces the row's content (lead, label, shortcut). The row element, role and state classes stay. */
  renderItem?: (item: MenuItem, state: { index: number; active: boolean }) => ReactNode;
  /** Render the fixed-width lead column (check mark or icon). Default true. */
  showLead?: boolean;
  /**
   * Focus the menu on open. Default true. When false the menu does not take focus (the document keeps
   * its selection and caret), and arrows/Home/End/Enter/Space/Escape/Tab are handled from the window
   * while the menu is open, so it stays keyboard operable. Chords with Ctrl/Cmd/Alt pass through.
   */
  autoFocus?: boolean;
  /**
   * Keyboard model. `"menu"` (default): roving highlight with arrows, Home, End, Enter, Space and Tab
   * handled by the component. `"escape-only"`: the component handles only Escape (close, focus
   * restored), sets no highlight and captures no other key anywhere, so Tab, arrows, Space and Enter
   * stay native. Items must then be real buttons: it requires `itemAs="button"` (a dev-mode warning
   * fires otherwise) and the buttons stay in the Tab order. Escape is also read from the window while
   * the menu is open, with an IME-composition guard.
   */
  keyboard?: "menu" | "escape-only";
  /** Row highlighted on open: `"first"` (default, first enabled row) or `"none"` (ArrowDown/Up start from the ends). */
  initialHighlight?: "first" | "none";
  /**
   * Exit animation length in ms. Default 0: unmount immediately. When above 0, closing first adds
   * `exitClassName` to the menu and scrim and calls `onExitStart`, then calls `onClose` after `exitMs`.
   * A chosen item's `run` fires at once, not after the animation. Input is ignored while exiting.
   * Re-rendering with a new `x`, `y` or `items` array during the exit counts as a new open and cancels
   * the exit, so pass a stable `items` (useMemo) or use `useContextMenu`, which remounts per open.
   */
  exitMs?: number;
  /** Class added to the menu and scrim while exiting. Default `is-closing`. */
  exitClassName?: string;
  /** Called once when an exit animation starts (only when `exitMs` > 0). */
  onExitStart?: () => void;
}

const enabled = (items: readonly MenuItem[]) => items.map((item, i) => (item.disabled ? -1 : i)).filter((i) => i >= 0);

/**
 * App-owned context menu. The shell never shows webview default menus; each app decides where a
 * menu exists and what is in it. Keyboard: arrows/Home/End move, Enter/Space activate, Escape
 * closes and restores focus. Mouse-down never steals the document selection (Copy/Cut still work).
 */
export function ContextMenu({
  x,
  y,
  items,
  onClose,
  label,
  width = 200,
  className,
  slots,
  scrimClassName,
  itemAs = "div",
  renderItem,
  showLead = true,
  autoFocus = true,
  initialHighlight = "first",
  keyboard = "menu",
  exitMs = 0,
  exitClassName = "is-closing",
  onExitStart,
}: ContextMenuProps) {
  const ref = useRef<HTMLDivElement>(null);
  const menuId = useRef(`rk-menu-${Math.random().toString(36).slice(2, 8)}`);
  const opener = useRef<HTMLElement | null>(typeof document !== "undefined" ? (document.activeElement as HTMLElement | null) : null);
  const escapeOnly = keyboard === "escape-only";
  const [active, setActive] = useState(() => (initialHighlight === "none" || escapeOnly ? -1 : enabled(items)[0] ?? -1));
  const [closing, setClosing] = useState(false);
  const closingRef = useRef(false);
  const exitTimer = useRef<number | null>(null);
  const [pos, setPos] = useState(() =>
    floatingPanelStyle({ x, y, width, estimatedHeight: items.length * 28 + 12, viewportWidth: window.innerWidth, viewportHeight: window.innerHeight }),
  );

  // Re-fit with the real height once laid out.
  useLayoutEffect(() => {
    const h = ref.current?.offsetHeight;
    if (h) setPos(floatingPanelStyle({ x, y, width, estimatedHeight: h, viewportWidth: window.innerWidth, viewportHeight: window.innerHeight }));
  }, [x, y, width, items]);

  /**
   * Single exit path. Without `exitMs` it is the 0.5.x sequence: onClose, restore focus, then `then`.
   * With `exitMs` the menu plays its exit first and only unmounts (onClose) afterwards; focus is
   * restored and `then` runs immediately so the chosen action never waits for an animation.
   */
  const dismiss = useCallback(
    (options?: { restoreFocus?: boolean; then?: () => void }) => {
      if (closingRef.current) return;
      if (!(exitMs > 0)) {
        onClose();
        if (options?.restoreFocus) opener.current?.focus?.();
        options?.then?.();
        return;
      }
      closingRef.current = true;
      setClosing(true);
      onExitStart?.();
      if (options?.restoreFocus) opener.current?.focus?.();
      options?.then?.();
      exitTimer.current = window.setTimeout(onClose, exitMs);
    },
    [exitMs, onClose, onExitStart],
  );

  useEffect(
    () => () => {
      if (exitTimer.current != null) window.clearTimeout(exitTimer.current);
    },
    [],
  );

  // A directly rendered menu that is re-opened while exiting is a new menu: drop the old exit (its
  // timer would otherwise close the new one). Keyed on x, y and the `items` array identity, so with
  // exitMs the host must pass a stable array (useMemo) or give the element a per-open `key`;
  // `useContextMenu` does both.
  const lastOpen = useRef({ x, y, items });
  useEffect(() => {
    const last = lastOpen.current;
    if (last.x === x && last.y === y && last.items === items) return;
    lastOpen.current = { x, y, items };
    if (exitTimer.current != null) window.clearTimeout(exitTimer.current);
    exitTimer.current = null;
    closingRef.current = false;
    setClosing(false);
    setActive(initialHighlight === "none" || escapeOnly ? -1 : enabled(items)[0] ?? -1);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [x, y, items]);

  useEffect(() => {
    if (autoFocus) ref.current?.focus({ preventScroll: true });
    const onResize = () => onClose();
    window.addEventListener("resize", onResize);
    window.addEventListener("blur", onResize);
    return () => {
      window.removeEventListener("resize", onResize);
      window.removeEventListener("blur", onResize);
    };
  }, [onClose, autoFocus]);

  useEffect(() => {
    document.getElementById(`${menuId.current}-${active}`)?.scrollIntoView?.({ block: "nearest" });
  }, [active]);

  const run = (item: MenuItem) => {
    if (item.disabled) return;
    dismiss({ restoreFocus: true, then: () => item.run?.() });
  };

  const handleKey = (e: { key: string; preventDefault: () => void; stopPropagation: () => void }) => {
    if (closingRef.current) return;
    if (escapeOnly) {
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        dismiss({ restoreFocus: true });
      }
      return;
    }
    const idx = enabled(items);
    const at = idx.indexOf(active);
    if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      dismiss({ restoreFocus: true });
    } else if (e.key === "ArrowDown") {
      e.preventDefault();
      setActive(idx[(at + 1) % idx.length] ?? -1);
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setActive(at < 0 ? idx[idx.length - 1] ?? -1 : idx[(at - 1 + idx.length) % idx.length] ?? -1);
    } else if (e.key === "Home") {
      e.preventDefault();
      setActive(idx[0] ?? -1);
    } else if (e.key === "End") {
      e.preventDefault();
      setActive(idx[idx.length - 1] ?? -1);
    } else if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      if (active >= 0) run(items[active]);
    } else if (e.key === "Tab") {
      e.preventDefault();
      dismiss({ restoreFocus: true });
    }
  };

  // Without autoFocus the menu never holds focus, so the same keys are read from the window while it is open.
  const keyHandler = useRef(handleKey);
  keyHandler.current = handleKey;
  useEffect(() => {
    if (autoFocus && !escapeOnly) return;
    const onWindowKey = (e: KeyboardEvent) => {
      if (e.ctrlKey || e.metaKey || e.altKey || e.defaultPrevented) return;
      if (e.isComposing || e.keyCode === 229) return; // IME composition: never act on or swallow these keys
      if (ref.current?.contains(e.target as Node | null)) return; // the menu's own onKeyDown already handled it
      keyHandler.current(e);
    };
    window.addEventListener("keydown", onWindowKey, true);
    return () => window.removeEventListener("keydown", onWindowKey, true);
  }, [autoFocus, escapeOnly]);

  useEffect(() => {
    if (escapeOnly && itemAs !== "button" && (globalThis as { process?: { env?: { NODE_ENV?: string } } }).process?.env?.NODE_ENV !== "production") {
      console.warn('[app-shell] ContextMenu keyboard="escape-only" relies on native button semantics; set itemAs="button".');
    }
  }, [escapeOnly, itemAs]);

  const onKeyDown = (e: React.KeyboardEvent) => {
    if (e.nativeEvent.isComposing || e.nativeEvent.keyCode === 229) return;
    handleKey(e);
  };
  const exitStyle = exitMs > 0 ? ({ "--rk-menu-exit-ms": `${exitMs}ms` } as React.CSSProperties) : undefined;
  const Row = itemAs;

  return createPortal(
    <>
      <div
        className={cx("rk-menu-scrim", scrimClassName, slots?.scrim, closing && exitClassName)}
        onMouseDown={() => dismiss()}
        onContextMenu={(e) => { e.preventDefault(); dismiss(); }}
      />
      <div
        ref={ref}
        role="menu"
        aria-label={label}
        aria-activedescendant={active >= 0 ? `${menuId.current}-${active}` : undefined}
        tabIndex={-1}
        className={cx("rk-menu", className, slots?.root, closing && exitClassName)}
        style={exitStyle ? { ...pos, ...exitStyle } : pos}
        onMouseDown={(e) => e.preventDefault()}
        onKeyDown={onKeyDown}
      >
        {items.map((item, i) => (
          <div key={item.id}>
            {item.separatorBefore ? <div className={cx("rk-menu__sep", slots?.separator)} role="separator" /> : null}
            <Row
              {...(itemAs === "button" ? { type: "button" as const, disabled: item.disabled, ...(escapeOnly ? {} : { tabIndex: -1 }) } : {})}
              id={`${menuId.current}-${i}`}
              role={item.checked === undefined ? "menuitem" : "menuitemcheckbox"}
              aria-checked={item.checked}
              aria-disabled={itemAs === "div" ? item.disabled || undefined : undefined}
              className={cx("rk-menu__item", i === active && "is-active", item.danger && "is-danger", item.disabled && "is-disabled", slots?.item)}
              onMouseMove={() => !escapeOnly && !item.disabled && setActive(i)}
              onClick={() => run(item)}
            >
              {renderItem ? (
                renderItem(item, { index: i, active: i === active })
              ) : (
                <>
                  {showLead ? <span className={cx("rk-menu__lead", slots?.lead)} aria-hidden="true">{item.checked ? "✓" : item.icon}</span> : null}
                  <span className={cx("rk-menu__label", slots?.label)}>{item.label}</span>
                  {item.shortcut ? <Kbd chord={item.shortcut} gap={1} /> : null}
                </>
              )}
            </Row>
          </div>
        ))}
      </div>
    </>,
    document.body,
  );
}

export interface OpenMenu<T> {
  /** Open counter; each open mounts a fresh `ContextMenu` (so a re-open during an exit animation is a new menu). */
  n?: number;
  x: number;
  y: number;
  context: T;
}

/**
 * `const cm = useContextMenu<Row>((row) => items, "Row actions")`, then
 * `<div onContextMenu={(e) => cm.open(e, row)} />` and render `{cm.element}` once.
 */
export function useContextMenu<T = undefined>(
  buildItems: (context: T) => readonly MenuItem[],
  label: string,
  /** Extra `ContextMenu` options (itemAs, autoFocus, exitMs, slots ...). */
  menuProps?: Omit<ContextMenuProps, "x" | "y" | "items" | "label" | "onClose">,
) {
  const [menu, setMenu] = useState<OpenMenu<T> | null>(null);
  const opens = useRef(0);
  const close = useCallback(() => setMenu(null), []);
  const open = useCallback((event: ReactMouseEvent | MouseEvent, context?: T) => {
    event.preventDefault();
    event.stopPropagation();
    setMenu({ n: ++opens.current, x: event.clientX, y: event.clientY, context: context as T });
  }, []);
  const items = useMemo(() => (menu ? buildItems(menu.context) : []), [menu, buildItems]);
  const element = menu && items.length ? <ContextMenu key={menu.n} {...menuProps} x={menu.x} y={menu.y} items={items} label={label} onClose={close} /> : null;
  return { open, close, isOpen: menu !== null, element };
}
