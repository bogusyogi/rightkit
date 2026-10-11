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

/** Options for {@link useFocusTrap}. Every default reproduces the 0.2.1 behaviour exactly. */
export interface FocusTrapOptions {
  /** Focus the first control on open. Default true. */
  autoFocus?: boolean;
  /** Handle Tab at all. `false` keeps focus-on-open and restore but leaves Tab native. Default true. */
  trap?: boolean;
  /** Restore focus to the opener on close. Default true. */
  restore?: boolean;
  /**
   * When the restore runs after the trap closes: `"microtask"` (default, after React finished the
   * unmount), `"sync"` (inside the effect cleanup), or a number of milliseconds (setTimeout).
   * Every timing is StrictMode-safe: it restores only when the container is gone or `active` is false.
   */
  restoreTiming?: "microtask" | "sync" | number;
  /**
   * Where the opener is read. `"render"` (default): during the first render, before React's own
   * `autoFocus` attribute moves focus at commit. `"effect"`: when the trap activates, immediately
   * before the first-control focus.
   */
  captureOpener?: "render" | "effect";
  /**
   * Tab pressed while focus is outside the container (keydown reached it anyway). `"both"` (default)
   * wraps Tab to the first and Shift+Tab to the last control; `"backward"` wraps only Shift+Tab;
   * `"none"` wraps neither. Tab on the last control and Shift+Tab on the first always wrap.
   */
  fromOutside?: "both" | "backward" | "none";
  /** Ignore `[hidden]` controls when finding the first and last. Default true. */
  skipHidden?: boolean;
  /** Tab with no focusable control: `"prevent"` (default) swallows it, `"pass"` leaves it native. */
  emptyList?: "prevent" | "pass";
  /** With no focusable control on open: `"container"` (default) focuses the container, `"none"` does nothing. */
  focusFallback?: "container" | "none";
}

/** Framework-free core of {@link useFocusTrap}: attach to `el`, return the detach (and restore) function. */
export function attachFocusTrap(
  el: HTMLElement,
  config: Required<Omit<FocusTrapOptions, "captureOpener">> & { opener: HTMLElement | null; isActive: () => boolean },
): () => void {
  const { autoFocus, trap, restore, restoreTiming, fromOutside, skipHidden, emptyList, focusFallback, opener, isActive } = config;
  const items = () => {
    const all = Array.from(el.querySelectorAll<HTMLElement>(FOCUSABLE));
    return skipHidden ? all.filter((n) => !n.hasAttribute("hidden")) : all;
  };
  if (autoFocus) (items()[0] ?? (focusFallback === "container" ? el : undefined))?.focus();
  const onKeyDown = (event: KeyboardEvent) => {
    if (event.key !== "Tab") return;
    const focusable = items();
    if (!focusable.length) {
      if (emptyList === "prevent") event.preventDefault();
      return;
    }
    const first = focusable[0];
    const last = focusable[focusable.length - 1];
    const current = el.ownerDocument.activeElement;
    const outside = !el.contains(current);
    if (event.shiftKey && (current === first || (outside && fromOutside !== "none"))) {
      event.preventDefault();
      last.focus();
    } else if (!event.shiftKey && (current === last || (outside && fromOutside === "both"))) {
      event.preventDefault();
      first.focus();
    }
  };
  if (trap) el.addEventListener("keydown", onKeyDown);
  return () => {
    if (trap) el.removeEventListener("keydown", onKeyDown);
    if (!restore) return;
    // Restore only on a real close. React StrictMode runs this cleanup against a still-mounted
    // dialog and would otherwise yank focus back to the opener.
    const restoreFocus = () => {
      if (!el.isConnected || !isActive()) opener?.focus?.();
    };
    if (restoreTiming === "sync") restoreFocus();
    else if (typeof restoreTiming === "number") setTimeout(restoreFocus, restoreTiming);
    else queueMicrotask(restoreFocus);
  };
}

/**
 * Trap Tab inside `ref` while `active`, focus the first control on open (unless `autoFocus` is
 * false) and restore focus to the opener on close. See {@link FocusTrapOptions}; with no options
 * this is the 0.2.1 behaviour.
 */
export function useFocusTrap(ref: RefObject<HTMLElement | null>, active = true, opts: FocusTrapOptions = {}) {
  const {
    autoFocus = true,
    trap = true,
    restore = true,
    restoreTiming = "microtask",
    captureOpener = "render",
    fromOutside = "both",
    skipHidden = true,
    emptyList = "prevent",
    focusFallback = "container",
  } = opts;
  // Captured during the first render: React's `autoFocus` attribute moves focus at commit, before
  // any effect runs, so reading activeElement inside the effect would find the dialog's own button.
  const activeRef = useLatest(active);
  const opener = useRef<HTMLElement | null>(typeof document === "undefined" ? null : (document.activeElement as HTMLElement | null));
  useEffect(() => {
    const el = ref.current;
    if (!active || !el) return;
    return attachFocusTrap(el, {
      autoFocus,
      trap,
      restore,
      restoreTiming,
      fromOutside,
      skipHidden,
      emptyList,
      focusFallback,
      opener: captureOpener === "effect" ? (document.activeElement as HTMLElement | null) : opener.current,
      isActive: () => activeRef.current,
    });
  }, [ref, active, autoFocus, trap, restore, restoreTiming, captureOpener, fromOutside, skipHidden, emptyList, focusFallback, activeRef]);
}

/**
 * Modal/popover dismiss state. `closing` flags the exit animation; `close` plays it once then calls
 * `onClose`. With `escape: true` a capture-phase Escape closes (stopPropagation, so a stacked layer
 * beneath does not also dismiss).
 */
export function useModalDismiss(
  onClose: () => void,
  opts?: {
    escape?: boolean;
    exitMs?: number;
    /** Call `preventDefault()` on the Escape that closes. Default true. */
    preventDefault?: boolean;
    /** Call `stopPropagation()` on the Escape that closes. Default true. */
    stopPropagation?: boolean;
  },
) {
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
  const prevent = opts?.preventDefault ?? true;
  const stop = opts?.stopPropagation ?? true;
  useEffect(() => {
    if (!escape) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        if (stop) e.stopPropagation();
        if (prevent) e.preventDefault();
        close();
      }
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [escape, close, prevent, stop]);
  return { closing, close };
}
