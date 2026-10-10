import { useCallback, useEffect, useRef, useState } from "react";
import { defaultToastBus, type Toast, type ToastBus, type ToastKind } from "../toast.js";
import { cx } from "./ui.js";
import type { SlotClasses } from "./slots.js";

export type ToastSlot = "root" | "toast" | "message" | "action" | "dismiss";

const GLYPH: Record<ToastKind, string> = { info: "i", success: "✓", warning: "!", error: "!" };
const EXIT_MS = 150;
const MAX_VISIBLE = 4;

function ToastRow({ toast, leaving, onDismiss, slots }: { toast: Toast; leaving: boolean; onDismiss: () => void; slots?: SlotClasses<ToastSlot> }) {
  return (
    <div role={toast.kind === "error" ? "alert" : "status"} data-kind={toast.kind} className={cx("rk-toast", `rk-toast--${toast.kind}`, leaving && "is-leaving", slots?.toast)}>
      <span className="rk-toast__glyph" aria-hidden="true">{GLYPH[toast.kind]}</span>
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
  /** Extra classes per region: `root` (live region), `toast`, `message`, `action`, `dismiss`. */
  slots?: SlotClasses<ToastSlot>;
}

export function ToastViewport({ bus = defaultToastBus, className, slots }: ToastViewportProps) {
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
    }, EXIT_MS));
  }, []);

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

  return (
    <div className={cx("rk-toasts", className, slots?.root)} aria-live="polite" aria-atomic="false" data-rk-toasts>
      {toasts.map((toast) => (
        <ToastRow key={toast.id} toast={toast} leaving={leaving.has(toast.id)} onDismiss={() => dismiss(toast.id)} slots={slots} />
      ))}
    </div>
  );
}
