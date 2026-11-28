// A native <dialog> driven by an `open` flag: `showModal()` owns the focus trap and Escape, the previously
// focused element gets focus back on close, and focus lands on the primary action whenever the visible state
// changes. Imports only `react`.
// Moved from HeardRight `components/model-update/ModelUpdatePrompt.tsx`.
import { useEffect, useRef, type KeyboardEvent, type MouseEvent, type RefObject, type SyntheticEvent } from 'react';

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
  /**
   * Dismisses once: calls `UseModalDialogOptions.onDismiss`, then `restoreFocus()`. A second call while the first
   * is in effect does nothing (the guard clears when `open` turns true again). Use it for the dialog's own buttons.
   */
  dismiss: () => void;
  /**
   * Spread on the `<dialog>`: `onClose` (the native close event, except the one StrictMode's replay raises, see
   * `closeOnUnmount`), `onCancel` (Escape the browser handles), `onClick` (a click on the backdrop, that is on the
   * dialog element itself) and `onKeyDown` (Escape): each ends in `dismiss()`.
   */
  dialogProps: ModalDialogProps;
}

/** The handlers {@link UseModalDialog.dialogProps} gives a native `<dialog>`. */
export interface ModalDialogProps {
  onClose: () => void;
  onCancel: (event: SyntheticEvent<HTMLDialogElement>) => void;
  onClick: (event: MouseEvent<HTMLDialogElement>) => void;
  onKeyDown: (event: KeyboardEvent<HTMLDialogElement>) => void;
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
  /**
   * With `closeOnUnmount`, whether unmounting a dialog that is still open also restores focus to the
   * opener. Default true. A host that restores focus itself when it dismisses the dialog, and moves focus
   * to a new page when the dialog goes away with a route change, sets false so unmount only closes it.
   * A dialog already closed when the component unmounts never restores focus on unmount.
   */
  restoreFocusOnUnmount?: boolean;
  /**
   * What `dismiss()` and `dialogProps` do to close the dialog from the host's side, usually setting its state so the
   * dialog unmounts or `open` goes false. Called once per dismissal, before focus is restored. Default: nothing.
   */
  onDismiss?: () => void;
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

  // StrictMode's dev-only double mount runs the unmount cleanup below while the dialog element stays mounted. Its
  // `el.close()` raises a `close` event that would call `onDismiss` and unmount the host's modal for nothing, so
  // `dialogProps.onClose` swallows exactly one close event raised by that cleanup. On a real unmount the element is
  // detached and the event never reaches React. Declared before the cleanup below so the flag is set before it closes
  // the element; the deferred reset only writes while mounted, so a real unmount leaves no task touching a dead ref.
  const suppressCleanupClose = useRef(false);
  const mounted = useRef(false);
  const closing = useRef(false);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      suppressCleanupClose.current = true;
      // The dispatched `close` event settles before this task runs. On StrictMode's remount `mounted` is already
      // true again and the reset must land; on a real unmount it stays false and the dead flag is left alone.
      window.setTimeout(() => {
        if (mounted.current) suppressCleanupClose.current = false;
      }, 0);
    };
  }, []);

  useEffect(() => {
    if (open) closing.current = false;
  }, [open]);

  const dismiss = useRef(() => {
    if (closing.current) return;
    closing.current = true;
    optionsRef.current.onDismiss?.();
    restoreFocus();
  }).current;

  const dialogProps = useRef<ModalDialogProps>({
    onClose: () => {
      if (suppressCleanupClose.current) {
        suppressCleanupClose.current = false;
        return;
      }
      dismiss();
    },
    onCancel: (event) => {
      event.preventDefault();
      dismiss();
    },
    // A click that lands on the dialog element itself (not a descendant) is a click on the native `::backdrop`.
    onClick: (event) => {
      if (event.target === dialogRef.current) dismiss();
    },
    onKeyDown: (event) => {
      if (event.key === 'Escape') {
        event.preventDefault();
        dismiss();
      }
    },
  }).current;

  // Opt-in unmount cleanup. The opener is kept (not cleared) so a StrictMode replay of the setup
  // effect above reopens the dialog and still restores to the original launcher.
  const closeOnUnmount = options.closeOnUnmount === true;
  useEffect(() => {
    if (!closeOnUnmount) return undefined;
    const el = dialogRef.current;
    return () => {
      const stillOpen = el?.open === true;
      if (el && stillOpen) el.close();
      if (stillOpen && optionsRef.current.restoreFocusOnUnmount !== false) restoreFocus();
    };
  }, [closeOnUnmount, restoreFocus]);

  return { dialogRef, primaryRef, restoreFocus, dismiss, dialogProps };
}
