// A native <dialog> driven by an `open` flag: `showModal()` owns the focus trap and Escape, the previously
// focused element gets focus back on close, and focus lands on the primary action whenever the visible state
// changes. Imports only `react`.
// Moved from HeardRight `components/model-update/ModelUpdatePrompt.tsx`.
import { useEffect, useRef, type RefObject } from 'react';

export interface UseModalDialog {
  dialogRef: RefObject<HTMLDialogElement | null>;
  /** Attach to the primary button of the current state; it receives focus when `stateKey` changes. */
  primaryRef: RefObject<HTMLButtonElement | null>;
  /**
   * Gives focus back to the element that was focused when the dialog opened, through
   * `UseModalDialogOptions.restoreFocus` when the host set one. For a host that closes the dialog
   * itself (a dismiss handler) and wants the same restore the hook does on `open` going false.
   */
  restoreFocus: () => void;
}

/** Options of {@link useModalDialog}. Every option is off by default: without them the hook behaves as before. */
export interface UseModalDialogOptions {
  /**
   * When true, unmounting the component closes the dialog if it is open and restores focus to the
   * opener. For a host that mounts the dialog only while it should show (mount means open, unmount
   * means closed) and so never flips `open` to false. Default false: unmount does nothing.
   */
  closeOnUnmount?: boolean;
  /**
   * `'browser'` leaves the focus `showModal()` picks on open and does not move it to `primaryRef`
   * when the dialog opens; later `stateKey` changes while open still focus `primaryRef`.
   * Default `'primary'`: focus the primary action on open.
   */
  initialFocus?: 'primary' | 'browser';
  /**
   * Replaces the default focus restore (`opener.focus()` when the opener is still connected). The
   * host passes a function taking the recorded opener (or null) and moving focus, for example to a
   * fallback destination when the opener is gone. Called on close through `open`, on unmount with
   * `closeOnUnmount`, and by the returned `restoreFocus()`.
   */
  restoreFocus?: (opener: HTMLElement | null) => void;
}

/**
 * @param open Whether the dialog is shown.
 * @param stateKey Any value that changes when the dialog moves to another state (a status string).
 * @param options Opt-in behaviours (`closeOnUnmount`, `initialFocus`, `restoreFocus`); none changes the default.
 */
export function useModalDialog(
  open: boolean,
  stateKey: string | number = '',
  options: UseModalDialogOptions = {},
): UseModalDialog {
  const dialogRef = useRef<HTMLDialogElement>(null);
  const primaryRef = useRef<HTMLButtonElement>(null);
  const opener = useRef<HTMLElement | null>(null);
  const optionsRef = useRef(options);
  optionsRef.current = options;
  const wasOpen = useRef(false);

  const restoreFocus = useRef(() => {
    const custom = optionsRef.current.restoreFocus;
    if (custom) custom(opener.current);
    else if (opener.current?.isConnected) opener.current.focus();
  }).current;

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
      restoreFocus();
      opener.current = null;
    }
  }, [open, restoreFocus]);

  useEffect(() => {
    void stateKey;
    const justOpened = open && !wasOpen.current;
    wasOpen.current = open;
    if (justOpened && optionsRef.current.initialFocus === 'browser') return;
    if (open) primaryRef.current?.focus();
  }, [open, stateKey]);

  // Opt-in unmount cleanup. The opener is kept (not cleared) so a StrictMode replay of the setup
  // effect above reopens the dialog and still restores to the original launcher.
  const closeOnUnmount = options.closeOnUnmount === true;
  useEffect(() => {
    if (!closeOnUnmount) return undefined;
    const el = dialogRef.current;
    return () => {
      if (el?.open) el.close();
      restoreFocus();
    };
  }, [closeOnUnmount, restoreFocus]);

  return { dialogRef, primaryRef, restoreFocus };
}
