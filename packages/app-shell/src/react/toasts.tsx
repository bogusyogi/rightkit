import { useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import { defaultToastBus, type Toast, type ToastBus, type ToastKind } from "../toast.js";
import { cx } from "./ui.js";
import type { SlotClasses } from "./slots.js";

export type ToastSlot = "root" | "toast" | "message" | "action" | "dismiss" | "glyph";

const GLYPH: Record<ToastKind, string> = { info: "i", success: "✓", warning: "!", error: "!" };
const EXIT_MS = 150;
const MAX_VISIBLE = 4;

/** Everything a custom row needs. Spread `role`, `data-kind` and `className` on your row element. */
export interface ToastRowContext {
  toast: Toast;
  /** True while the exit animation runs (`exitMs`). */
  leaving: boolean;
  dismiss: () => void;
  /** Resolved role for this kind (see `roles`), undefined when the kind has none. */
  role: "alert" | "status" | "log" | undefined;
  /** The classes the built-in row would carry (rk classes, state, `rowClassName`, `exitClassName`, `slots.toast`). */
  className: string;
}

/** Role per kind. `null` omits the role. Default: `alert` for error, `status` for the rest. */
export type ToastRoles = Partial<Record<ToastKind, "alert" | "status" | "log" | null>>;

interface RowOptions {
  slots?: SlotClasses<ToastSlot>;
  roles?: ToastRoles;
  rowClassName?: string | ((toast: Toast, state: { leaving: boolean }) => string | undefined);
  exitClassName?: string;
  renderGlyph?: (toast: Toast) => ReactNode;
  renderRow?: (row: ToastRowContext) => ReactNode;
}

function ToastRow({ toast, leaving, onDismiss, options }: { toast: Toast; leaving: boolean; onDismiss: () => void; options: RowOptions }) {
  const { slots, roles, rowClassName, exitClassName, renderGlyph, renderRow } = options;
  const configured = roles?.[toast.kind];
  const role = configured === undefined ? (toast.kind === "error" ? "alert" : "status") : configured ?? undefined;
  const extra = typeof rowClassName === "function" ? rowClassName(toast, { leaving }) : rowClassName;
  const className = cx("rk-toast", `rk-toast--${toast.kind}`, leaving && "is-leaving", leaving && exitClassName, extra, slots?.toast);
  if (renderRow) return <>{renderRow({ toast, leaving, dismiss: onDismiss, role, className })}</>;
  return (
    <div role={role} data-kind={toast.kind} className={className}>
      {renderGlyph ? renderGlyph(toast) : <span className={cx("rk-toast__glyph", slots?.glyph)} aria-hidden="true">{GLYPH[toast.kind]}</span>}
      <span className={cx("rk-toast__msg", slots?.message)}>{toast.message}</span>
      {toast.action ? (
        <button type="button" className={cx("rk-toast__action", slots?.action)} onClick={() => { toast.action?.run(); onDismiss(); }}>{toast.action.label}</button>
      ) : null}
      <button type="button" className={cx("rk-toast__x", slots?.dismiss)} aria-label="Dismiss notification" onClick={onDismiss}>✕</button>
    </div>
  );
}

/** Single mount point for a toast bus: a polite live region, errors announce as alerts. */
export interface ToastViewportProps {
  bus?: ToastBus;
  /** Extra class on the live region. */
  className?: string;
  /** Extra classes per region: `root` (live region), `toast`, `message`, `action`, `dismiss`, `glyph`. */
  slots?: SlotClasses<ToastSlot>;
  /** Role per kind; `null` omits it. Default: error is `alert`, the others `status`. */
  roles?: ToastRoles;
  /** Extra class on every row, or a function of the toast and its exiting state. */
  rowClassName?: string | ((toast: Toast, state: { leaving: boolean }) => string | undefined);
  /** Class added to a row while it exits, next to `is-leaving`. */
  exitClassName?: string;
  /** Exit animation length in ms before the row is removed. Default 150. */
  exitMs?: number;
  /** Replaces the glyph element (`span.rk-toast__glyph`) of each row. Return `null` for none. */
  renderGlyph?: (toast: Toast) => ReactNode;
  /** Replaces the whole row. Use `role`, `className` and `dismiss` from the context; focus and Escape stay yours. */
  renderRow?: (row: ToastRowContext) => ReactNode;
}

export function ToastViewport({ bus = defaultToastBus, className, slots, roles, rowClassName, exitClassName, exitMs = EXIT_MS, renderGlyph, renderRow }: ToastViewportProps) {
  const [toasts, setToasts] = useState<Toast[]>([]);
  const [leaving, setLeaving] = useState<ReadonlySet<number>>(() => new Set());
  const timers = useRef(new Map<number, number>());

  const dismiss = useCallback((id: number) => {
    const ttl = timers.current.get(id);
    if (ttl != null) window.clearTimeout(ttl);
    setLeaving((prev) => (prev.has(id) ? prev : new Set(prev).add(id)));
    timers.current.set(id, window.setTimeout(() => {
      setToasts((t) => t.filter((x) => x.id !== id));
      setLeaving((d) => {
        const n = new Set(d);
        n.delete(id);
        return n;
      });
      timers.current.delete(id);
    }, exitMs));
  }, [exitMs]);

  useEffect(() => {
    const pending = timers.current;
    const off = bus.subscribe((event) => {
      if (event.type === "dismiss") return dismiss(event.id);
      const { toast } = event;
      setToasts((prev) => [...prev.slice(-(MAX_VISIBLE - 1)), toast]);
      if (toast.ttl > 0) pending.set(toast.id, window.setTimeout(() => dismiss(toast.id), toast.ttl));
    });
    return () => {
      off();
      for (const t of pending.values()) window.clearTimeout(t);
      pending.clear();
    };
  }, [bus, dismiss]);

  const rowOptions: RowOptions = { slots, roles, rowClassName, exitClassName, renderGlyph, renderRow };
  return (
    <div className={cx("rk-toasts", className, slots?.root)} aria-live="polite" aria-atomic="false" data-rk-toasts>
      {toasts.map((toast) => (
        <ToastRow key={toast.id} toast={toast} leaving={leaving.has(toast.id)} onDismiss={() => dismiss(toast.id)} options={rowOptions} />
      ))}
    </div>
  );
}
