// Shared pieces of a list's remove-with-undo flow: a confirm dialog opened with `showModal`, an undo snapshot
// that expires, and focus that lands on the next remove button. Imports only `react`.
import { useCallback, useEffect, useRef, useState, type RefObject } from "react";

export interface UseRemovalFlowOptions {
  /** How long the undo offer stays. Default 6000. */
  undoWindowMs?: number;
}

export interface RemovalFlow<T, U = T> {
  pending: T | null;
  /** Ask for confirmation to remove `item`. */
  request: (item: T) => void;
  /** Close the confirmation without removing. */
  cancel: () => void;
  /** Close the confirmation and return the item that was pending. */
  take: () => T | null;
  /** Snapshot offered for undo. */
  undo: U | null;
  /** Offer undo for `snapshot` until the window ends. */
  offerUndo: (snapshot: U) => void;
  /** Stop offering undo and return the snapshot. */
  takeUndo: () => U | null;
  /** Attach to the confirm `<dialog>`; it is shown modally while `pending` is set. */
  dialogRef: RefObject<HTMLDialogElement | null>;
}

export function useRemovalFlow<T, U = T>(options: UseRemovalFlowOptions = {}): RemovalFlow<T, U> {
  const undoWindowMs = options.undoWindowMs ?? 6000;
  const [pending, setPending] = useState<T | null>(null);
  const [undo, setUndo] = useState<U | null>(null);
  const undoRef = useRef<U | null>(null);
  const pendingRef = useRef<T | null>(null);
  const undoTimerRef = useRef<number | undefined>(undefined);
  const dialogRef = useRef<HTMLDialogElement>(null);
  undoRef.current = undo;
  pendingRef.current = pending;

  useEffect(() => {
    return () => {
      if (undoTimerRef.current !== undefined) window.clearTimeout(undoTimerRef.current);
    };
  }, []);

  useEffect(() => {
    const el = dialogRef.current;
    if (!pending) return;
    el?.showModal();
    return () => el?.close();
  }, [pending]);

  const request = useCallback((item: T) => setPending(item), []);
  const cancel = useCallback(() => setPending(null), []);
  const take = useCallback(() => {
    const item = pendingRef.current;
    setPending(null);
    return item;
  }, []);
  const offerUndo = useCallback(
    (snapshot: U) => {
      if (undoTimerRef.current !== undefined) window.clearTimeout(undoTimerRef.current);
      setUndo(snapshot);
      undoTimerRef.current = window.setTimeout(() => {
        undoTimerRef.current = undefined;
        setUndo(null);
      }, undoWindowMs);
    },
    [undoWindowMs],
  );
  const takeUndo = useCallback(() => {
    const snapshot = undoRef.current;
    if (!snapshot) return null;
    if (undoTimerRef.current !== undefined) window.clearTimeout(undoTimerRef.current);
    setUndo(null);
    return snapshot;
  }, []);

  return { pending, request, cancel, take, undo, offerUndo, takeUndo, dialogRef };
}

/** Focuses the remove button now at `index` (clamped), or `fallback` when none are left. */
export function focusRemoveButton(selector: string, index: number, fallback: RefObject<HTMLElement | null>): void {
  const removeButtons = document.querySelectorAll<HTMLButtonElement>(selector);
  const nextButton = removeButtons.length > 0 ? removeButtons.item(Math.min(index, removeButtons.length - 1)) : null;
  if (nextButton) nextButton.focus();
  else fallback.current?.focus();
}
