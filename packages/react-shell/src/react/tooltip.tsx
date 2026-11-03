import { cloneElement, useEffect, useId, useRef, useState, type FocusEvent, type PointerEvent, type ReactElement, type ReactNode } from "react";
import { cx } from "./ui.js";

type AnchorProps = {
  "aria-describedby"?: string;
  onPointerEnter?: (event: PointerEvent<HTMLElement>) => void;
  onPointerLeave?: (event: PointerEvent<HTMLElement>) => void;
  onFocus?: (event: FocusEvent<HTMLElement>) => void;
  onBlur?: (event: FocusEvent<HTMLElement>) => void;
};

export interface TooltipProps {
  /** The text of the tooltip. It is also the accessible description of the anchor while it is shown. */
  content: ReactNode;
  /** Side of the anchor the tooltip sits on. Default `top`. */
  placement?: "top" | "bottom" | "start" | "end";
  /** Wait before a pointer hover shows the tooltip. Focus shows it at once. Default 400. */
  delayMs?: number;
  className?: string;
  /** One focusable element, typically an icon-only button. Its own handlers still run. */
  children: ReactElement<AnchorProps>;
}

/**
 * A `role="tooltip"` label for an icon-only control. It shows on pointer hover (after `delayMs`) and on keyboard focus,
 * is tied to the anchor with `aria-describedby` while shown, and is dismissed by Escape and by pointer leave. Its
 * fade is removed under reduced motion through the shell's motion rules.
 */
export function Tooltip({ content, placement = "top", delayMs = 400, className, children }: TooltipProps) {
  const id = useId();
  const [open, setOpen] = useState(false);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);

  const cancel = () => {
    if (timer.current !== null) clearTimeout(timer.current);
    timer.current = null;
  };

  useEffect(() => cancel, []);

  useEffect(() => {
    if (!open) return;
    const onKey = (event: globalThis.KeyboardEvent) => {
      if (event.key === "Escape") setOpen(false);
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [open]);

  const anchor = children.props;
  const handled = cloneElement(children, {
    "aria-describedby": open ? id : anchor["aria-describedby"],
    onPointerEnter: (event: PointerEvent<HTMLElement>) => {
      anchor.onPointerEnter?.(event);
      cancel();
      timer.current = setTimeout(() => {
        timer.current = null;
        setOpen(true);
      }, delayMs);
    },
    onPointerLeave: (event: PointerEvent<HTMLElement>) => {
      anchor.onPointerLeave?.(event);
      cancel();
      setOpen(false);
    },
    onFocus: (event: FocusEvent<HTMLElement>) => {
      anchor.onFocus?.(event);
      cancel();
      setOpen(true);
    },
    onBlur: (event: FocusEvent<HTMLElement>) => {
      anchor.onBlur?.(event);
      cancel();
      setOpen(false);
    },
  });

  return (
    <span className={cx("rk-tooltip-anchor", className)}>
      {handled}
      {open ? (
        <span role="tooltip" id={id} className="rk-tooltip" data-placement={placement}>
          {content}
        </span>
      ) : null}
    </span>
  );
}
