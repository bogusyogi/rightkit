// A native <dialog> driven by an `open` flag: `showModal()` owns the focus trap and Escape, the previously
// focused element gets focus back on close, and focus lands on the primary action whenever the visible state
// changes. Imports only `react`.
// Moved from HeardRight `components/model-update/ModelUpdatePrompt.tsx`.
import { useEffect, useRef, type RefObject } from 'react';

export interface UseModalDialog {
  dialogRef: RefObject<HTMLDialogElement | null>;
  /** Attach to the primary button of the current state; it receives focus when `stateKey` changes. */
  primaryRef: RefObject<HTMLButtonElement | null>;
}

/**
 * @param open Whether the dialog is shown.
 * @param stateKey Any value that changes when the dialog moves to another state (a status string).
 */
export function useModalDialog(open: boolean, stateKey: string | number = ''): UseModalDialog {
  const dialogRef = useRef<HTMLDialogElement>(null);
  const primaryRef = useRef<HTMLButtonElement>(null);
  const opener = useRef<HTMLElement | null>(null);

  useEffect(() => {
    const el = dialogRef.current;
    if (!el) return;
    if (open && !el.open) {
      if (!opener.current && document.activeElement instanceof HTMLElement) {
        opener.current = document.activeElement;
      }
      el.showModal();
    } else if (!open && el.open) {
      el.close();
      if (opener.current?.isConnected) opener.current.focus();
      opener.current = null;
    }
  }, [open]);

  useEffect(() => {
    void stateKey;
    if (open) primaryRef.current?.focus();
  }, [open, stateKey]);

  return { dialogRef, primaryRef };
}
