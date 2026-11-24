// Small event wiring for forms and confirm dialogs. Imports only `react`.
import { useEffect, useRef, type KeyboardEvent, type RefObject } from "react";

/** An `onKeyDown` that calls `action` on Enter and ignores other keys. Pass `fn` as is: it is called with no arguments. */
export function onEnterKey(action: () => void): (event: KeyboardEvent<HTMLElement>) => void {
  return (event) => {
    if (event.key === "Enter") action();
  };
}

/** The props `removalDialogProps` returns for the confirm `<dialog>`. */
export interface RemovalDialogProps {
  ref: RefObject<HTMLDialogElement | null>;
  /** The native cancel event (Escape): the default close is prevented and `cancel` runs. */
  onCancel(event: { preventDefault(): void }): void;
  /** A click on the backdrop (the dialog element itself, not its content) cancels. */
  onClick(event: { target: EventTarget }): void;
  /** Escape cancels where the cancel event does not fire. */
  onKeyDown(event: KeyboardEvent<HTMLDialogElement>): void;
}

/**
 * Props for the confirm `<dialog>` of a remove flow (`useRemovalFlow().dialogRef` and `.cancel`): Escape and a backdrop
 * click cancel instead of closing the dialog natively, so the pending item and the dialog never disagree. Spread onto the
 * `<dialog>`; add `aria-labelledby` and the classes yourself.
 */
export function removalDialogProps(flow: { dialogRef: RefObject<HTMLDialogElement | null>; cancel(): void }): RemovalDialogProps {
  return {
    ref: flow.dialogRef,
    onCancel: (event) => {
      event.preventDefault();
      flow.cancel();
    },
    onClick: (event) => {
      if (event.target === flow.dialogRef.current) flow.cancel();
    },
    onKeyDown: (event) => {
      if (event.key === "Escape") {
        event.preventDefault();
        flow.cancel();
      }
    },
  };
}

/** A ref for an element that takes focus whenever `active` becomes true (a rename field when renaming starts). */
export function useFocusWhen<T extends HTMLElement = HTMLInputElement>(active: boolean): RefObject<T | null> {
  const ref = useRef<T | null>(null);
  useEffect(() => {
    if (active) ref.current?.focus();
  }, [active]);
  return ref;
}
