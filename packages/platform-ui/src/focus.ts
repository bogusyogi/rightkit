/* Headless React hooks (no DOM assumptions beyond window/document). Re-exported by @rightkit/app-shell. */
import { useCallback, useEffect, useRef, useState, type RefObject } from "react";

const FOCUSABLE =
  'a[href],button:not([disabled]),input:not([disabled]),select:not([disabled]),textarea:not([disabled]),[tabindex]:not([tabindex="-1"])';

/** Latest-value ref so effects and listeners never capture stale props. */
export function useLatest<T>(value: T): RefObject<T> {
  const ref = useRef(value);
  ref.current = value;
  return ref;
}

/**
 * Trap Tab inside `ref` while `active`, focus the first control on open (unless `autoFocus` is
 * false) and restore focus to the opener on close.
 */
export function useFocusTrap(ref: RefObject<HTMLElement | null>, active = true, opts: { autoFocus?: boolean } = {}) {
  const { autoFocus = true } = opts;
  // Captured during the first render: React's `autoFocus` attribute moves focus at commit, before
  // any effect runs, so reading activeElement inside the effect would find the dialog's own button.
  const activeRef = useLatest(active);
  const opener = useRef<HTMLElement | null>(typeof document === "undefined" ? null : (document.activeElement as HTMLElement | null));
  useEffect(() => {
    const el = ref.current;
    if (!active || !el) return;
    const previouslyFocused = opener.current;
    const items = () => Array.from(el.querySelectorAll<HTMLElement>(FOCUSABLE)).filter((n) => !n.hasAttribute("hidden"));
    if (autoFocus) (items()[0] ?? el).focus();
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key !== "Tab") return;
      const focusable = items();
      if (!focusable.length) {
        event.preventDefault();
        return;
      }
      const first = focusable[0];
      const last = focusable[focusable.length - 1];
      const current = document.activeElement;
      if (event.shiftKey && (current === first || !el.contains(current))) {
        event.preventDefault();
        last.focus();
      } else if (!event.shiftKey && (current === last || !el.contains(current))) {
        event.preventDefault();
        first.focus();
      }
    };
    el.addEventListener("keydown", onKeyDown);
    return () => {
      el.removeEventListener("keydown", onKeyDown);
      // Restore only on a real close. React StrictMode runs this cleanup against a still-mounted
      // dialog and would otherwise yank focus back to the opener.
      queueMicrotask(() => {
        if (!el.isConnected || !activeRef.current) previouslyFocused?.focus?.();
      });
    };
  }, [ref, active, autoFocus, activeRef]);
}

/**
 * Modal/popover dismiss state. `closing` flags the exit animation; `close` plays it once then calls
 * `onClose`. With `escape: true` a capture-phase Escape closes (stopPropagation, so a stacked layer
 * beneath does not also dismiss).
 */
export function useModalDismiss(onClose: () => void, opts?: { escape?: boolean; exitMs?: number }) {
  const [closing, setClosing] = useState(false);
  const timer = useRef<number | null>(null);
  const onCloseRef = useLatest(onClose);
  const exitMs = opts?.exitMs ?? 80;
  const closingRef = useRef(false);
  const close = useCallback(() => {
    if (closingRef.current) return;
    closingRef.current = true;
    setClosing(true);
    timer.current = window.setTimeout(() => onCloseRef.current?.(), exitMs);
  }, [exitMs, onCloseRef]);
  useEffect(() => () => {
    if (timer.current != null) window.clearTimeout(timer.current);
  }, []);
  const escape = opts?.escape ?? false;
  useEffect(() => {
    if (!escape) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        e.preventDefault();
        close();
      }
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [escape, close]);
  return { closing, close };
}
