import { useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import { defaultToastBus, type Toast, type ToastBus, type ToastKind } from "../toast.js";
import { cx } from "./ui.js";
import { isImplicitDuplicate } from "./toast-dedupe.js";
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
export type ToastPlacement = "top-center" | "bottom-center" | "top-end";

export interface ToastViewportProps {
  bus?: ToastBus;
  /**
   * Where the stack sits. Default `bottom-center` (the 0.6.x position, markup unchanged). `top-center` sets
   * `data-placement="top-center"` on the live region; `--rk-toast-top-offset` (default 16px) is the distance from the top edge.
   * `top-end` sets `data-placement="top-end"`: top right, `--rk-toast-top-end-offset` (default 3rem) from the top and
   * `--rk-toast-end-offset` (default 3.5rem) from the right edge.
   */
  placement?: ToastPlacement;
  /**
   * `"newest"`: a toast whose kind, message and action label equal the newest visible row does not add a second row;
   * the row stays and its timer restarts. A failing poller that repeats one sentence shows one toast. Default: no de-duplication.
   */
  dedupe?: "newest";
  /** Most rows shown at once; the oldest is dropped when a new one arrives. Default 4. */
  maxVisible?: number;
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

export function ToastViewport({ bus = defaultToastBus, placement, className, slots, roles, rowClassName, exitClassName, exitMs = EXIT_MS, renderGlyph, renderRow, dedupe, maxVisible = MAX_VISIBLE }: ToastViewportProps) {
  const [toasts, setToasts] = useState<Toast[]>([]);
  const [leaving, setLeaving] = useState<ReadonlySet<number>>(() => new Set());
  const timers = useRef(new Map<number, number>());
  // Mirrors of the state, so a bus event that arrives in the same tick sees the rows it must compare with.
  const rows = useRef<Toast[]>([]);
  const leavingIds = useRef<Set<number>>(new Set());
  // Read at call time so changing `exitMs` never re-runs the subscription effect (which clears pending timers).
  const exitMsRef = useRef(exitMs);
  exitMsRef.current = exitMs;
  const optionsRef = useRef({ dedupe, maxVisible });
  optionsRef.current = { dedupe, maxVisible };

  const commit = useCallback((next: Toast[]) => {
    rows.current = next;
    setToasts(next);
  }, []);

  // Start the exit animation and remove the row afterwards. Never touches the bus.
  const beginLeave = useCallback((id: number) => {
    if (leavingIds.current.has(id) || !rows.current.some((x) => x.id === id)) return;
    const ttl = timers.current.get(id);
    if (ttl != null) window.clearTimeout(ttl);
    leavingIds.current.add(id);
    setLeaving((prev) => (prev.has(id) ? prev : new Set(prev).add(id)));
    timers.current.set(id, window.setTimeout(() => {
      commit(rows.current.filter((x) => x.id !== id));
      leavingIds.current.delete(id);
      setLeaving((d) => {
        const n = new Set(d);
        n.delete(id);
        return n;
      });
      timers.current.delete(id);
    }, exitMsRef.current));
  }, [commit]);

  // Viewport-initiated removal (timer, close button, action): leave, then release the bus bookkeeping.
  // The bus echoes a dismiss event back; beginLeave ignores it because the row is already leaving.
  const dismiss = useCallback((id: number) => {
    beginLeave(id);
    bus.dismiss(id);
  }, [bus, beginLeave]);

  useEffect(() => {
    const pending = timers.current;
    const ids = leavingIds.current;
    const off = bus.subscribe((event) => {
      if (event.type === "dismiss") return beginLeave(event.id);
      const { toast } = event;
      const newest = rows.current[rows.current.length - 1];
      if (optionsRef.current.dedupe === "newest" && newest && !ids.has(newest.id) && isImplicitDuplicate(newest, toast)) {
        const running = pending.get(newest.id);
        if (running != null) window.clearTimeout(running);
        pending.delete(newest.id);
        if (toast.ttl > 0) pending.set(newest.id, window.setTimeout(() => dismiss(newest.id), toast.ttl));
        // The replacement never gets a row, so release its bus entry now or dedupe would count it live forever.
        bus.dismiss(toast.id);
        return;
      }
      const max = Math.max(1, optionsRef.current.maxVisible);
      const keep = rows.current.slice(Math.max(0, rows.current.length - (max - 1)));
      const evicted = rows.current.filter((x) => !keep.includes(x));
      commit([...keep, toast]);
      for (const row of evicted) {
        const running = pending.get(row.id);
        if (running != null) window.clearTimeout(running);
        pending.delete(row.id);
        ids.delete(row.id);
      }
      setLeaving((prev) => (evicted.some((x) => prev.has(x.id)) ? new Set([...prev].filter((id) => !evicted.some((x) => x.id === id))) : prev));
      for (const row of evicted) bus.dismiss(row.id);
      if (toast.ttl > 0) pending.set(toast.id, window.setTimeout(() => dismiss(toast.id), toast.ttl));
    });
    return () => {
      off();
      for (const t of pending.values()) window.clearTimeout(t);
      pending.clear();
      // Unmount: release every row's bus entry (the subscription is already off, so nothing echoes back).
      for (const row of rows.current) bus.dismiss(row.id);
      ids.clear();
      rows.current = [];
      setToasts([]);
      setLeaving(new Set());
    };
  }, [bus, dismiss, beginLeave, commit]);

  const rowOptions: RowOptions = { slots, roles, rowClassName, exitClassName, renderGlyph, renderRow };
  return (
    <div className={cx("rk-toasts", className, slots?.root)} aria-live="polite" aria-atomic="false" data-rk-toasts data-placement={placement}>
      {toasts.map((toast) => (
        <ToastRow key={toast.id} toast={toast} leaving={leaving.has(toast.id)} onDismiss={() => dismiss(toast.id)} options={rowOptions} />
      ))}
    </div>
  );
}
