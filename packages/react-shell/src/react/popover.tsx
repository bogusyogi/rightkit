import { useEffect, useLayoutEffect, useRef, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { useDismiss } from "@rightkit/platform-ui/headless";
import { useLatest } from "./hooks.js";
import { anchorPoint, type MenuPlacement } from "./contextmenu.js";
import { floatingPanelStyle, type FloatingPanelStyle } from "../geometry.js";
import { cx } from "./ui.js";
import type { SlotClasses } from "./slots.js";

export interface PopoverProps {
  /**
   * The element the panel opens from. It is ignored by outside-press dismissal, so a click on the opener reaches the
   * opener's own handler (typically a toggle) instead of closing and reopening the panel.
   */
  anchor: Element | null;
  /** Called on Escape, an outside press, window resize or scroll outside the panel. The owner unmounts the popover. */
  onClose: () => void;
  /** Accessible name of the panel. */
  label: string;
  /** Side of the anchor the panel opens on. Default `bottom-start`. */
  placement?: MenuPlacement;
  /** Panel width in px. Default 240. */
  width?: number;
  /** Extra class on the panel. */
  className?: string;
  /** Extra class for the panel region (same as `className`). */
  slots?: SlotClasses<"root">;
  children: ReactNode;
}

/**
 * Anchored floating panel for arbitrary content (a filter form, a colour picker, a small list). Takes focus on open,
 * closes on Escape or an outside press, and restores focus to the opener when focus was inside the panel. Positioned
 * below or above its anchor (flipped when it would overflow), like `ContextMenu` with `anchor`.
 */
export function Popover({ anchor, onClose, label, placement = "bottom-start", width = 240, className, slots, children }: PopoverProps) {
  const panelRef = useRef<HTMLDivElement>(null);
  const anchorRef = useRef<Element | null>(anchor);
  anchorRef.current = anchor;
  const onCloseRef = useLatest(onClose);
  const opener = useRef<Element | null>(typeof document !== "undefined" ? document.activeElement : null);
  const [pos, setPos] = useState<FloatingPanelStyle | null>(null);

  useDismiss(() => onCloseRef.current(), { container: panelRef, ignore: [anchorRef], onResize: true, onScroll: true });

  // Place from the anchor's rectangle, then re-fit with the panel's real height once it is laid out.
  useLayoutEffect(() => {
    const rect = anchor?.getBoundingClientRect();
    if (!rect) return;
    const viewport = { width: window.innerWidth, height: window.innerHeight };
    const height = panelRef.current?.offsetHeight || 120;
    const at = anchorPoint({ left: rect.left, top: rect.top, right: rect.right, bottom: rect.bottom }, placement, { width, height }, viewport);
    setPos(floatingPanelStyle({ x: at.x, y: at.y, width, estimatedHeight: height, viewportWidth: viewport.width, viewportHeight: viewport.height }));
  }, [anchor, placement, width]);

  useEffect(() => {
    panelRef.current?.focus({ preventScroll: true });
    const restore = opener.current as HTMLElement | null;
    return () => {
      // Hand focus back only when it was inside the panel (or lost with it), never from a click elsewhere.
      const active = typeof document !== "undefined" ? document.activeElement : null;
      if (restore?.isConnected && (!active || active === document.body || panelRef.current?.contains(active))) restore.focus?.({ preventScroll: true });
    };
  }, []);

  return createPortal(
    <div ref={panelRef} role="dialog" aria-label={label} tabIndex={-1} className={cx("rk-popover", className, slots?.root)} style={pos ?? undefined}>
      {children}
    </div>,
    document.body,
  );
}
