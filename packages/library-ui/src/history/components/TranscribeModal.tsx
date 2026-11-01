import { lazy, Suspense, useEffect, useRef } from "react";

import { focusConnectedOrPageDestination } from "@rightkit/dictation-ui/lib/settingsRegistry";
import { cx } from "@rightkit/dictation-ui/lib/ui";

// Split out of the entry chunk with the other non-critical surfaces — the
// Transcribe page parses on first modal open, not at every window load.
const TranscribeSurface = lazy(() =>
  import("../pages/Transcribe.js").then((m) => ({ default: m.TranscribeSurface })),
);

// Native `<dialog>` focus trap: `showModal()` (not the `open` attribute) is
// what makes the browser own focus containment, Escape-to-cancel, and the
// `::backdrop` layer - a `<dialog open>` alone stays a plain non-modal block
// with background content still reachable by mouse/keyboard. App mounts this
// component only while the modal should be open, so mount == "should be open"
// and `showModal()` is called imperatively from a mount effect.
export default function TranscribeModal({
  isMac,
  isPro,
  onClose,
}: {
  isMac: boolean;
  isPro: boolean | null;
  onClose: () => void;
}) {
  const dialogRef = useRef<HTMLDialogElement>(null);
  const closeButtonRef = useRef<HTMLButtonElement>(null);
  // React StrictMode's dev-only double-mount runs the mount effect's cleanup
  // while the dialog element stays mounted. `el.close()` in that cleanup
  // dispatches a `close` event that would call the parent's onClose and
  // unmount the modal spuriously, so the handler swallows exactly one close
  // event raised by cleanup. On a real unmount the element detaches and the
  // event never reaches React's root listener anyway.
  const suppressCleanupClose = useRef(false);
  // Tracks the live mount across the StrictMode double-mount below: the
  // cleanup's deferred flag reset only writes while mounted, so a real
  // unmount leaves no task mutating a dead ref.
  const mounted = useRef(false);
  const openerRef = useRef<HTMLElement | null>(null);
  const closing = useRef(false);

  useEffect(() => {
    mounted.current = true;
    // StrictMode replays setup after dialog focus has moved inside. Keep its
    // first launcher instead of replacing it with the close control.
    if (!openerRef.current) {
      openerRef.current = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    }
    const el = dialogRef.current;
    // Second StrictMode setup pass: the cleanup already closed the element,
    // so reopen it. `showModal()` on an open dialog throws InvalidStateError.
    if (!el?.open) el?.showModal();
    // Match the pre-existing focus contract: land on the close button, not
    // whatever `showModal()`'s own focus algorithm would pick inside
    // `TranscribeSurface`.
    closeButtonRef.current?.focus();
    return () => {
      mounted.current = false;
      suppressCleanupClose.current = true;
      el?.close();
      // The dispatched `close` event settles before this task runs (and on a
      // real unmount it never dispatches to React at all). On StrictMode's
      // remount `mounted` is already true again and the reset must land; on
      // a real unmount it stays false and the dead flag is left alone.
      window.setTimeout(() => {
        if (mounted.current) suppressCleanupClose.current = false;
      }, 0);
    };
  }, []);

  const dismiss = () => {
    if (closing.current) return;
    closing.current = true;
    onClose();
    focusConnectedOrPageDestination(openerRef.current);
  };

  const handleDialogClose = () => {
    if (suppressCleanupClose.current) {
      suppressCleanupClose.current = false;
      return;
    }
    dismiss();
  };

  return (
    <dialog
      ref={dialogRef}
      aria-labelledby="transcribe-dialog-title"
      className="fixed inset-0 m-auto flex h-fit max-h-[88vh] w-[min(680px,calc(100vw-32px))] flex-col rounded-[var(--radius-lg)] border border-[var(--glass-edge)] bg-[var(--surface-1)] p-[var(--space-5)] text-[var(--paper)] [box-shadow:0_24px_80px_rgba(0,0,0,0.45),inset_0_1px_0_var(--glass-highlight)] backdrop:bg-[rgba(0,0,0,0.42)] backdrop:[backdrop-filter:blur(12px)]"
      onClose={handleDialogClose}
      onClick={(e) => {
        // A click that lands on the dialog element itself (not a descendant)
        // is a click on the native `::backdrop` - close it, mirroring the old
        // manual backdrop `<button>`'s outside-click-to-close.
        if (e.target === dialogRef.current) dismiss();
      }}
      onCancel={(e) => {
        e.preventDefault();
        dismiss();
      }}
      onKeyDown={(e) => {
        if (e.key === "Escape") {
          e.preventDefault();
          dismiss();
        }
      }}
    >
      <div className="mb-[var(--space-4)] flex shrink-0 items-start justify-between gap-[var(--space-4)]">
        <div>
          <h2
            id="transcribe-dialog-title"
            className="m-0 [font-family:var(--font-display)] text-[length:var(--size-xl)] leading-tight text-[var(--paper)]"
          >
            Transcribe a File
          </h2>
          <p className="m-0 mt-[var(--space-1)] text-[length:var(--size-sm)] text-[var(--muted)]">
            On-device. Your audio is never uploaded.
          </p>
        </div>
        <button
          ref={closeButtonRef}
          type="button"
          className={cx(
            "grid cursor-pointer place-items-center border-0 leading-none transition-[background-color,transform,box-shadow] duration-150 ease-[var(--ease-out-strong)] focus-visible:outline-none focus-visible:[box-shadow:var(--focus-ring)] active:scale-[0.94]",
            isMac
              ? "h-[18px] w-[18px] rounded-full bg-[#ff5f57] text-[#601316] text-[13px] font-bold [box-shadow:inset_0_0_0_1px_rgba(0,0,0,.14)] hover:bg-[#ff6b64]"
              : "h-9 w-9 rounded-[9px] bg-transparent text-[var(--muted)] text-[22px] hover:bg-[var(--surface-hover)] hover:text-[var(--paper)]",
          )}
          aria-label="Close transcribe dialog"
          title="Close"
          onClick={dismiss}
        >
          ×
        </button>
      </div>
      <Suspense fallback={null}>
        <TranscribeSurface compact isPro={isPro} />
      </Suspense>
    </dialog>
  );
}
