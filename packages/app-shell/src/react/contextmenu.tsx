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
  /** Extra classes per region: `root` (menu), `item`, `separator`, `label`. */
  slots?: SlotClasses<"root" | "item" | "separator" | "label">;
}

const enabled = (items: readonly MenuItem[]) => items.map((item, i) => (item.disabled ? -1 : i)).filter((i) => i >= 0);

/**
 * App-owned context menu. The shell never shows webview default menus; each app decides where a
 * menu exists and what is in it. Keyboard: arrows/Home/End move, Enter/Space activate, Escape
 * closes and restores focus. Mouse-down never steals the document selection (Copy/Cut still work).
 */
export function ContextMenu({ x, y, items, onClose, label, width = 200, className, slots }: ContextMenuProps) {
  const ref = useRef<HTMLDivElement>(null);
  const menuId = useRef(`rk-menu-${Math.random().toString(36).slice(2, 8)}`);
  const opener = useRef<HTMLElement | null>(typeof document !== "undefined" ? (document.activeElement as HTMLElement | null) : null);
  const [active, setActive] = useState(() => enabled(items)[0] ?? -1);
  const [pos, setPos] = useState(() =>
    floatingPanelStyle({ x, y, width, estimatedHeight: items.length * 28 + 12, viewportWidth: window.innerWidth, viewportHeight: window.innerHeight }),
  );

  // Re-fit with the real height once laid out.
  useLayoutEffect(() => {
    const h = ref.current?.offsetHeight;
    if (h) setPos(floatingPanelStyle({ x, y, width, estimatedHeight: h, viewportWidth: window.innerWidth, viewportHeight: window.innerHeight }));
  }, [x, y, width, items]);

  const close = useCallback(() => {
    onClose();
    opener.current?.focus?.();
  }, [onClose]);

  useEffect(() => {
    ref.current?.focus({ preventScroll: true });
    const onResize = () => onClose();
    window.addEventListener("resize", onResize);
    window.addEventListener("blur", onResize);
    return () => {
      window.removeEventListener("resize", onResize);
      window.removeEventListener("blur", onResize);
    };
  }, [onClose]);

  useEffect(() => {
    document.getElementById(`${menuId.current}-${active}`)?.scrollIntoView?.({ block: "nearest" });
  }, [active]);

  const run = (item: MenuItem) => {
    if (item.disabled) return;
    onClose();
    opener.current?.focus?.();
    item.run?.();
  };

  const onKeyDown = (e: React.KeyboardEvent) => {
    const idx = enabled(items);
    const at = idx.indexOf(active);
    if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      close();
    } else if (e.key === "ArrowDown") {
      e.preventDefault();
      setActive(idx[(at + 1) % idx.length] ?? -1);
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setActive(idx[(at - 1 + idx.length) % idx.length] ?? -1);
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
      close();
    }
  };

  return createPortal(
    <>
      <div className="rk-menu-scrim" onMouseDown={onClose} onContextMenu={(e) => { e.preventDefault(); onClose(); }} />
      <div
        ref={ref}
        role="menu"
        aria-label={label}
        aria-activedescendant={active >= 0 ? `${menuId.current}-${active}` : undefined}
        tabIndex={-1}
        className={cx("rk-menu", className, slots?.root)}
        style={pos}
        onMouseDown={(e) => e.preventDefault()}
        onKeyDown={onKeyDown}
      >
        {items.map((item, i) => (
          <div key={item.id}>
            {item.separatorBefore ? <div className={cx("rk-menu__sep", slots?.separator)} role="separator" /> : null}
            <div
              id={`${menuId.current}-${i}`}
              role={item.checked === undefined ? "menuitem" : "menuitemcheckbox"}
              aria-checked={item.checked}
              aria-disabled={item.disabled || undefined}
              className={cx("rk-menu__item", i === active && "is-active", item.danger && "is-danger", item.disabled && "is-disabled", slots?.item)}
              onMouseMove={() => !item.disabled && setActive(i)}
              onClick={() => run(item)}
            >
              <span className="rk-menu__lead" aria-hidden="true">{item.checked ? "✓" : item.icon}</span>
              <span className={cx("rk-menu__label", slots?.label)}>{item.label}</span>
              {item.shortcut ? <Kbd chord={item.shortcut} gap={1} /> : null}
            </div>
          </div>
        ))}
      </div>
    </>,
    document.body,
  );
}

export interface OpenMenu<T> {
  x: number;
  y: number;
  context: T;
}

/**
 * `const cm = useContextMenu<Row>((row) => items, "Row actions")`, then
 * `<div onContextMenu={(e) => cm.open(e, row)} />` and render `{cm.element}` once.
 */
export function useContextMenu<T = undefined>(buildItems: (context: T) => readonly MenuItem[], label: string) {
  const [menu, setMenu] = useState<OpenMenu<T> | null>(null);
  const close = useCallback(() => setMenu(null), []);
  const open = useCallback((event: ReactMouseEvent | MouseEvent, context?: T) => {
    event.preventDefault();
    event.stopPropagation();
    setMenu({ x: event.clientX, y: event.clientY, context: context as T });
  }, []);
  const items = useMemo(() => (menu ? buildItems(menu.context) : []), [menu, buildItems]);
  const element = menu && items.length ? <ContextMenu x={menu.x} y={menu.y} items={items} label={label} onClose={close} /> : null;
  return { open, close, isOpen: menu !== null, element };
}
