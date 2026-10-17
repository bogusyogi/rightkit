/**
 * Dependency-free toast bus (ViewRight's pattern, with CodeRight's variants + actions). Any module,
 * React or not, can `toast(...)`; the single `ToastViewport` subscribes and renders an aria-live stack.
 */
export type ToastKind = "info" | "success" | "warning" | "error";

export interface ToastAction {
  label: string;
  run: () => void;
}

export interface Toast {
  id: number;
  message: string;
  kind: ToastKind;
  action?: ToastAction;
  /** Replace-by-key identity given at push time (`toast(message, { id })`). */
  key?: string;
  /** Milliseconds until auto-dismiss; 0 keeps it until dismissed. */
  ttl: number;
}

export interface ToastOptions {
  kind?: ToastKind;
  action?: ToastAction;
  /**
   * Replace-by-key: a push with the same `id` as a live toast dismisses that toast first, then shows the new one.
   * Keyed toasts are never deduplicated.
   */
  id?: string;
  ttl?: number;
}

export interface ToastBusOptions {
  ttl?: Partial<Record<ToastKind, number>>;
  /**
   * When true, a push whose message and kind equal a live toast returns that toast's id and emits nothing. Only
   * plain toasts take part: a toast with an action, or with an `id`, is never deduplicated. Default false.
   */
  dedupe?: boolean;
  /**
   * Most live toasts at once. A push beyond it dismisses the oldest live toast first. Default unlimited.
   */
  max?: number;
}

export const DEFAULT_TOAST_TTL: Readonly<Record<ToastKind, number>> = { info: 3000, success: 3000, warning: 5000, error: 6000 };

export type ToastEvent = { type: "push"; toast: Toast } | { type: "dismiss"; id: number };

/**
 * A toast is "live" from its push until `dismiss(id)` is called on the bus. The viewport must therefore call
 * `bus.dismiss(id)` for every removal (timeout, close button, action), or dedupe and `max` see stale toasts.
 */
export function createToastBus(options: ToastBusOptions = {}) {
  let seq = 0;
  const listeners = new Set<(event: ToastEvent) => void>();
  const ttl = { ...DEFAULT_TOAST_TTL, ...options.ttl };
  const live = new Map<number, Toast>();
  const keyed = new Map<string, number>();
  const emit = (event: ToastEvent) => {
    for (const listener of listeners) listener(event);
  };
  const forget = (id: number): void => {
    const toast = live.get(id);
    if (!toast) return;
    live.delete(id);
    if (toast.key !== undefined && keyed.get(toast.key) === id) keyed.delete(toast.key);
  };
  return {
    push(message: string, opts: ToastOptions | ToastKind = {}): number {
      const o: ToastOptions = typeof opts === "string" ? { kind: opts } : opts;
      const kind = o.kind ?? "info";
      if (options.dedupe && o.id === undefined && o.action === undefined) {
        for (const existing of live.values()) {
          if (existing.message === message && existing.kind === kind && existing.action === undefined && existing.key === undefined) {
            return existing.id;
          }
        }
      }
      if (o.id !== undefined) {
        const previous = keyed.get(o.id);
        if (previous !== undefined) {
          forget(previous);
          emit({ type: "dismiss", id: previous });
        }
      }
      if (options.max !== undefined) {
        const limit = Math.max(1, options.max);
        while (live.size >= limit) {
          const oldest = live.keys().next().value as number;
          forget(oldest);
          emit({ type: "dismiss", id: oldest });
        }
      }
      const toast: Toast = { id: ++seq, message, kind, action: o.action, key: o.id, ttl: o.ttl ?? ttl[kind] };
      live.set(toast.id, toast);
      if (o.id !== undefined) keyed.set(o.id, toast.id);
      emit({ type: "push", toast });
      return toast.id;
    },
    /** Idempotent: drops the toast's bookkeeping and emits `dismiss` once; an id that is not live emits nothing. */
    dismiss(id: number): void {
      if (!live.has(id)) return;
      forget(id);
      emit({ type: "dismiss", id });
    },
    subscribe(listener: (event: ToastEvent) => void): () => void {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
  };
}

export type ToastBus = ReturnType<typeof createToastBus>;

/** Process-wide default bus so non-React code can announce without a provider. */
export const defaultToastBus: ToastBus = createToastBus();

export function toast(message: string, opts?: ToastOptions | ToastKind): number {
  return defaultToastBus.push(message, opts);
}
