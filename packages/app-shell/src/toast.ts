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
  /** Milliseconds until auto-dismiss; 0 keeps it until dismissed. */
  ttl: number;
}

export interface ToastOptions {
  kind?: ToastKind;
  action?: ToastAction;
  ttl?: number;
}

export const DEFAULT_TOAST_TTL: Readonly<Record<ToastKind, number>> = { info: 3000, success: 3000, warning: 5000, error: 6000 };

export type ToastEvent = { type: "push"; toast: Toast } | { type: "dismiss"; id: number };

export function createToastBus(options: { ttl?: Partial<Record<ToastKind, number>> } = {}) {
  let seq = 0;
  const listeners = new Set<(event: ToastEvent) => void>();
  const ttl = { ...DEFAULT_TOAST_TTL, ...options.ttl };
  return {
    push(message: string, opts: ToastOptions | ToastKind = {}): number {
      const o: ToastOptions = typeof opts === "string" ? { kind: opts } : opts;
      const kind = o.kind ?? "info";
      const toast: Toast = { id: ++seq, message, kind, action: o.action, ttl: o.ttl ?? ttl[kind] };
      for (const listener of listeners) listener({ type: "push", toast });
      return toast.id;
    },
    dismiss(id: number): void {
      for (const listener of listeners) listener({ type: "dismiss", id });
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
