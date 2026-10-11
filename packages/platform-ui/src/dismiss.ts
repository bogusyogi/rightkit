/* Headless dismissal: Escape, outside press, and optional window blur / resize / scroll. No markup, no styles. */
import { useEffect, type RefObject } from "react";
import { useLatest } from "./focus.js";

export type DismissReason = "escape" | "outside" | "blur" | "resize" | "scroll";

/**
 * How an outside press dismisses.
 * - `"mousedown"`: the button goes down outside.
 * - `"click"`: a click whose press AND release both landed outside (a drag that starts or ends inside never dismisses).
 * - `"click-target"`: any click whose target is outside, however the press began (a drag from inside the panel that
 *   ends on the backdrop dismisses, as a plain `onClick` on the backdrop does).
 */
export type OutsidePress = "mousedown" | "click" | "click-target";

export interface DismissOptions {
  /** Detach every listener while false. Default true. */
  enabled?: boolean;
  /** Escape dismisses. Default true. */
  escape?: boolean;
  /**
   * Where Escape is read. `"window"` (default): capture phase on the window, so it works wherever focus is and
   * runs before page handlers. A ref: bubble phase on that element only (e.g. the dialog root).
   */
  escapeTarget?: "window" | RefObject<EventTarget | null>;
  /** `preventDefault()` on the Escape that dismisses. Default true. */
  preventDefault?: boolean;
  /** `stopPropagation()` on the Escape that dismisses, so a layer beneath does not also close. Default true. */
  stopPropagation?: boolean;
  /** Ignore Escape while an IME composition is active (`isComposing` or `keyCode` 229). Default true. */
  imeGuard?: boolean;
  /** Outside-press mode, or `false` for none. Default `"mousedown"`. Needs `container` or `backdrop`. */
  outside?: false | OutsidePress;
  /** The panel. A target is outside when the panel does not contain it (and no `ignore` element does). */
  container?: RefObject<Element | null>;
  /** The backdrop element. When given, only a target that IS the backdrop counts as outside. */
  backdrop?: RefObject<Element | null>;
  /** Elements that never count as outside (the opener button). */
  ignore?: ReadonlyArray<RefObject<Element | null>>;
  /** A context-menu gesture outside dismisses and its native menu is suppressed. Default false. */
  outsideContextMenu?: boolean;
  /** Dismiss when the window loses focus. Default false. */
  onWindowBlur?: boolean;
  /** Dismiss when the window is resized. Default false. */
  onResize?: boolean;
  /** Dismiss when anything scrolls (capture on the window; scrolling inside `container` is ignored). Default false. */
  onScroll?: boolean;
}

type Target = Pick<EventTarget, "addEventListener" | "removeEventListener">;

export interface DismissConfig {
  onDismiss: (reason: DismissReason, event: Event) => void;
  escape: boolean;
  escapeTarget: Target | null;
  preventDefault: boolean;
  stopPropagation: boolean;
  imeGuard: boolean;
  outside: false | OutsidePress;
  outsideContextMenu: boolean;
  /** Resolved at event time. */
  isOutside: (target: EventTarget | null) => boolean;
  /** True when a scroll target is inside the panel. */
  isInsideScroll?: (target: EventTarget | null) => boolean;
  onWindowBlur: boolean;
  onResize: boolean;
  onScroll: boolean;
}

/** Framework-free core of {@link useDismiss}. Returns the detach function. `win` and `doc` default to the globals. */
export function attachDismiss(config: DismissConfig, env: { win?: Target; doc?: Target } = {}): () => void {
  const win = env.win ?? (globalThis as unknown as { window: Target }).window;
  const doc = env.doc ?? (globalThis as unknown as { document: Target }).document;
  const off: Array<() => void> = [];
  const on = (target: Target, type: string, fn: (event: Event) => void, capture = false) => {
    target.addEventListener(type, fn, capture);
    off.push(() => target.removeEventListener(type, fn, capture));
  };

  if (config.escape) {
    const onKey = (event: Event) => {
      const e = event as KeyboardEvent;
      if (e.key !== "Escape") return;
      if (config.imeGuard && (e.isComposing || e.keyCode === 229)) return;
      if (config.stopPropagation) e.stopPropagation();
      if (config.preventDefault) e.preventDefault();
      config.onDismiss("escape", e);
    };
    if (config.escapeTarget) on(config.escapeTarget, "keydown", onKey, false);
    else on(win, "keydown", onKey, true);
  }

  const mode = config.outside;
  if (mode) {
    let pressOutside = false;
    let releaseOutside = false;
    if (mode !== "click-target") {
      on(doc, "mousedown", (e) => {
        const outside = config.isOutside(e.target);
        if (mode === "mousedown") {
          if (outside) config.onDismiss("outside", e);
        } else {
          pressOutside = outside;
          releaseOutside = false;
        }
      }, true);
    }
    if (mode === "click") {
      on(doc, "mouseup", (e) => {
        releaseOutside = config.isOutside(e.target);
      }, true);
    }
    if (mode === "click" || mode === "click-target") {
      on(doc, "click", (e) => {
        if (mode === "click") {
          const both = pressOutside && releaseOutside;
          pressOutside = false;
          releaseOutside = false;
          if (both && config.isOutside(e.target)) config.onDismiss("outside", e);
        } else if (config.isOutside(e.target)) {
          config.onDismiss("outside", e);
        }
      }, true);
    }
  }
  if (config.outsideContextMenu) {
    on(doc, "contextmenu", (e) => {
      if (!config.isOutside(e.target)) return;
      e.preventDefault();
      config.onDismiss("outside", e);
    }, true);
  }
  if (config.onWindowBlur) on(win, "blur", (e) => config.onDismiss("blur", e));
  if (config.onResize) on(win, "resize", (e) => config.onDismiss("resize", e));
  if (config.onScroll) {
    on(win, "scroll", (e) => {
      if (config.isInsideScroll?.(e.target)) return;
      config.onDismiss("scroll", e);
    }, true);
  }
  return () => {
    for (const fn of off.splice(0)) fn();
  };
}

const contains = (ref: RefObject<Element | null> | undefined, target: EventTarget | null) => {
  const node = ref?.current;
  return !!node && !!target && node.contains(target as Node);
};

/**
 * Dismiss a popover, menu or modal. Calls `onDismiss(reason, event)`; it neither renders nor animates, so pair
 * it with `useExitTransition` for an exit. Listeners attach while `enabled` and are removed on close and unmount.
 */
export function useDismiss(onDismiss: (reason: DismissReason, event: Event) => void, options: DismissOptions = {}): void {
  const latestOptions = useLatest(options);
  const latestDismiss = useLatest(onDismiss);
  const {
    enabled = true,
    escape = true,
    escapeTarget = "window",
    preventDefault = true,
    stopPropagation = true,
    imeGuard = true,
    outside = "mousedown",
    outsideContextMenu = false,
    onWindowBlur = false,
    onResize = false,
    onScroll = false,
  } = options;
  useEffect(() => {
    if (!enabled) return;
    const o = () => latestOptions.current;
    const hasZone = !!(o().container || o().backdrop);
    const target = escapeTarget === "window" ? null : escapeTarget.current;
    return attachDismiss({
      onDismiss: (reason, event) => latestDismiss.current(reason, event),
      escape: escape && (escapeTarget === "window" || !!target),
      escapeTarget: target,
      preventDefault,
      stopPropagation,
      imeGuard,
      outside: hasZone ? outside : false,
      outsideContextMenu: hasZone && outsideContextMenu,
      isOutside: (t) => {
        const { backdrop, container, ignore } = o();
        if (!t) return false;
        if (backdrop) return backdrop.current === t;
        if (!container?.current) return false;
        return !contains(container, t) && !(ignore ?? []).some((ref) => contains(ref, t));
      },
      isInsideScroll: (t) => contains(o().container, t),
      onWindowBlur,
      onResize,
      onScroll,
    });
  }, [enabled, escape, escapeTarget, preventDefault, stopPropagation, imeGuard, outside, outsideContextMenu, onWindowBlur, onResize, onScroll, latestOptions, latestDismiss]);
}
