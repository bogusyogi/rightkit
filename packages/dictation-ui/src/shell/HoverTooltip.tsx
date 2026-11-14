import { useEffect, useRef, useState } from "react";

const SHOW_DELAY_MS = 240;
const TARGET_SELECTOR = 'button:not([disabled]),[role="button"]:not([aria-disabled="true"])';

type TooltipState = { text: string; x: number; y: number };

function tooltipTarget(node: EventTarget | null) {
  return node instanceof Element ? node.closest<HTMLElement>(TARGET_SELECTOR) : null;
}

const normalize = (value: string) => value.replace(/\s+/g, " ").trim();

// Only tooltip what the eye can't already read: icon-only buttons, or a label
// that adds to the visible text. A tooltip echoing a button's own visible
// label ("Commands", "Show All →") is noise.
function tooltipText(target: HTMLElement) {
  const explicit =
    target.dataset.tooltip ?? target.getAttribute("aria-label") ?? target.getAttribute("title");
  if (!explicit) return "";
  const text = normalize(explicit);
  const visible = normalize(target.innerText).toLowerCase();
  return visible && text.toLowerCase() === visible ? "" : text;
}

export default function HoverTooltip() {
  const [tooltip, setTooltip] = useState<TooltipState | null>(null);
  const activeRef = useRef<HTMLElement | null>(null);
  const timerRef = useRef(0);
  const pointRef = useRef({ x: 0, y: 0 });

  useEffect(() => {
    const clearTimer = () => {
      if (timerRef.current) window.clearTimeout(timerRef.current);
      timerRef.current = 0;
    };
    const restoreTitle = (target: HTMLElement | null) => {
      if (!target?.dataset.hrNativeTitle) return;
      target.setAttribute("title", target.dataset.hrNativeTitle);
      delete target.dataset.hrNativeTitle;
    };
    const hide = () => {
      clearTimer();
      restoreTitle(activeRef.current);
      activeRef.current = null;
      setTooltip(null);
    };
    const arm = (target: HTMLElement, x: number, y: number) => {
      const text = tooltipText(target);
      if (!text) return;
      hide();
      activeRef.current = target;
      pointRef.current = { x, y };
      const nativeTitle = target.getAttribute("title");
      if (nativeTitle) {
        target.dataset.hrNativeTitle = nativeTitle;
        target.removeAttribute("title");
      }
      timerRef.current = window.setTimeout(() => {
        timerRef.current = 0;
        setTooltip({ text, ...pointRef.current });
      }, SHOW_DELAY_MS);
    };
    const onPointerOver = (event: PointerEvent) => {
      const target = tooltipTarget(event.target);
      if (!target || target === activeRef.current) return;
      arm(target, event.clientX, event.clientY);
    };
    const onPointerMove = (event: PointerEvent) => {
      if (!activeRef.current) return;
      pointRef.current = { x: event.clientX, y: event.clientY };
    };
    const onPointerOut = (event: PointerEvent) => {
      const active = activeRef.current;
      if (!active || active.contains(event.relatedTarget as Node | null)) return;
      hide();
    };
    const onFocusIn = (event: FocusEvent) => {
      const target = tooltipTarget(event.target);
      if (!target) return;
      // Tooltips must only show with keyboard focus (:focus-visible), never on programmatic focus
      if (!target.matches(":focus-visible")) return;
      const rect = target.getBoundingClientRect();
      arm(target, rect.left + rect.width / 2, rect.bottom);
    };
    const onFocusOut = () => hide();

    document.addEventListener("pointerover", onPointerOver, true);
    document.addEventListener("pointermove", onPointerMove, true);
    document.addEventListener("pointerout", onPointerOut, true);
    document.addEventListener("focusin", onFocusIn, true);
    document.addEventListener("focusout", onFocusOut, true);
    return () => {
      hide();
      document.removeEventListener("pointerover", onPointerOver, true);
      document.removeEventListener("pointermove", onPointerMove, true);
      document.removeEventListener("pointerout", onPointerOut, true);
      document.removeEventListener("focusin", onFocusIn, true);
      document.removeEventListener("focusout", onFocusOut, true);
    };
  }, []);

  if (!tooltip) return null;
  return (
    <div
      role="tooltip"
      className="pointer-events-none fixed z-[100] max-w-[280px] -translate-x-1/2 translate-y-[14px] overflow-hidden text-ellipsis whitespace-nowrap rounded-[6px] border border-[var(--line)] bg-[var(--surface-2)] px-[9px] py-[6px] text-[12px] font-semibold leading-[1.1] text-[var(--paper)] [box-shadow:0_5px_18px_rgba(0,0,0,.32)]"
      style={{ left: tooltip.x, top: tooltip.y }}
    >
      {tooltip.text}
    </div>
  );
}
