// A page-wide hover and keyboard-focus tooltip: one tooltip for every matching element, found by event
// delegation on the document. The host draws the box (markup, styling) from the hook's output.
import { useEffect, useLayoutEffect, useRef, useState } from "react";

export interface TooltipBox {
  left: number;
  top: number;
  right: number;
  bottom: number;
}

export interface TooltipPlacementOptions {
  /** Gap between the anchor and the tooltip. Default 8. */
  gapPx?: number;
  /** Minimum distance kept from the window edge. Default 8. */
  edgePx?: number;
}

/**
 * Places a tooltip beside the element it describes, so it never covers that element or the rows above and
 * below it: to the right and vertically centred on it; else above it; else below it. Every choice is clamped
 * inside the window.
 */
export function placeTooltip(
  anchor: TooltipBox,
  width: number,
  height: number,
  options: TooltipPlacementOptions = {},
): { left: number; top: number } {
  const gap = options.gapPx ?? 8;
  const edge = options.edgePx ?? 8;
  const maxLeft = Math.max(edge, window.innerWidth - width - edge);
  const maxTop = Math.max(edge, window.innerHeight - height - edge);
  const clampLeft = (left: number) => Math.min(Math.max(left, edge), maxLeft);
  const clampTop = (top: number) => Math.min(Math.max(top, edge), maxTop);
  const centredX = (anchor.left + anchor.right) / 2 - width / 2;
  const centredY = (anchor.top + anchor.bottom) / 2 - height / 2;
  const rightOfAnchor = anchor.right + gap;
  if (rightOfAnchor + width + edge <= window.innerWidth) {
    return { left: rightOfAnchor, top: clampTop(centredY) };
  }
  const aboveAnchor = anchor.top - gap - height;
  if (aboveAnchor >= edge) return { left: clampLeft(centredX), top: aboveAnchor };
  return { left: clampLeft(centredX), top: clampTop(anchor.bottom + gap) };
}

const normalize = (value: string) => value.replace(/\s+/g, " ").trim();

/**
 * The tooltip text of an element: `data-tooltip`, else `aria-label`, else `title`. Empty when the element
 * already shows that text itself (a tooltip echoing a button's visible label is noise).
 */
export function tooltipTextFor(target: HTMLElement): string {
  const explicit = target.dataset.tooltip ?? target.getAttribute("aria-label") ?? target.getAttribute("title");
  if (!explicit) return "";
  const text = normalize(explicit);
  const visible = normalize(target.innerText).toLowerCase();
  return visible && text.toLowerCase() === visible ? "" : text;
}

export interface UseHoverTooltipOptions extends TooltipPlacementOptions {
  /** Delay before the tooltip shows on pointer hover. Default 240. */
  showDelayMs?: number;
  /** Elements that get a tooltip. Default: enabled buttons and `role="button"`. */
  targetSelector?: string;
  /**
   * `dataset` key under which the element's native `title` is parked while its tooltip is armed (so the
   * browser's own tooltip does not double it). Default `"rkNativeTitle"`.
   */
  nativeTitleKey?: string;
}

const DEFAULT_TARGET_SELECTOR = 'button:not([disabled]),[role="button"]:not([aria-disabled="true"])';

/**
 * State of the page-wide tooltip. Spread `boxRef` on the tooltip element; render it only while `text` is set,
 * at `placement` (hidden until measured). Pointer hover shows after a delay; keyboard focus
 * (`:focus-visible`) shows at once; programmatic focus never shows one.
 */
export function useHoverTooltip(options: UseHoverTooltipOptions = {}) {
  const {
    showDelayMs = 240,
    targetSelector = DEFAULT_TARGET_SELECTOR,
    nativeTitleKey = "rkNativeTitle",
    gapPx,
    edgePx,
  } = options;
  const [text, setText] = useState<string | null>(null);
  const activeRef = useRef<HTMLElement | null>(null);
  const timerRef = useRef(0);

  useEffect(() => {
    const targetOf = (node: EventTarget | null) =>
      node instanceof Element ? node.closest<HTMLElement>(targetSelector) : null;
    const clearTimer = () => {
      if (timerRef.current) window.clearTimeout(timerRef.current);
      timerRef.current = 0;
    };
    const restoreTitle = (target: HTMLElement | null) => {
      const parked = target?.dataset[nativeTitleKey];
      if (!target || !parked) return;
      target.setAttribute("title", parked);
      delete target.dataset[nativeTitleKey];
    };
    const hide = () => {
      clearTimer();
      restoreTitle(activeRef.current);
      activeRef.current = null;
      setText(null);
    };
    const arm = (target: HTMLElement) => {
      const next = tooltipTextFor(target);
      if (!next) return;
      hide();
      activeRef.current = target;
      const nativeTitle = target.getAttribute("title");
      if (nativeTitle) {
        target.dataset[nativeTitleKey] = nativeTitle;
        target.removeAttribute("title");
      }
      timerRef.current = window.setTimeout(() => {
        timerRef.current = 0;
        setText(next);
      }, showDelayMs);
    };
    const onPointerOver = (event: PointerEvent) => {
      const target = targetOf(event.target);
      if (!target || target === activeRef.current) return;
      arm(target);
    };
    const onPointerOut = (event: PointerEvent) => {
      const active = activeRef.current;
      if (!active || active.contains(event.relatedTarget as Node | null)) return;
      hide();
    };
    const onFocusIn = (event: FocusEvent) => {
      const target = targetOf(event.target);
      if (!target) return;
      // Tooltips must only show with keyboard focus (:focus-visible), never on programmatic focus
      if (!target.matches(":focus-visible")) return;
      arm(target);
    };
    const onFocusOut = () => hide();

    document.addEventListener("pointerover", onPointerOver, true);
    document.addEventListener("pointerout", onPointerOut, true);
    document.addEventListener("focusin", onFocusIn, true);
    document.addEventListener("focusout", onFocusOut, true);
    return () => {
      hide();
      document.removeEventListener("pointerover", onPointerOver, true);
      document.removeEventListener("pointerout", onPointerOut, true);
      document.removeEventListener("focusin", onFocusIn, true);
      document.removeEventListener("focusout", onFocusOut, true);
    };
  }, [showDelayMs, targetSelector, nativeTitleKey]);

  const boxRef = useRef<HTMLDivElement | null>(null);
  const [placement, setPlacement] = useState<{ left: number; top: number } | null>(null);
  // Measure the tooltip, then place it against the hovered element's box, not the pointer.
  useLayoutEffect(() => {
    const box = boxRef.current;
    const anchor = activeRef.current;
    if (text === null || !box || !anchor) {
      setPlacement(null);
      return;
    }
    const { width, height } = box.getBoundingClientRect();
    setPlacement(placeTooltip(anchor.getBoundingClientRect(), width, height, { gapPx, edgePx }));
  }, [text, gapPx, edgePx]);

  return { text, placement, boxRef };
}
