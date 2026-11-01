import { useEffect, useRef } from "react";

import { cx, ui } from "../lib/ui";

// Native `<dialog>` focus trap: `showModal()` (not the `open` attribute) is
// what makes the browser own focus containment, Escape-to-cancel, and the
// `::backdrop` layer - a `<dialog open>` alone stays a plain non-modal block
// with background content still reachable by mouse/keyboard. App mounts this
// component only while the notice should be visible, so mount == "should be
// open" and `showModal()` is called imperatively from a mount effect.
export default function LicenseLifecycleDialog({
  dialog,
  onSecondary,
  onPrimary,
}: {
  dialog: { title: string; body: string; primaryAction: string; secondaryAction: string };
  onSecondary: () => void;
  onPrimary: () => void;
}) {
  const dialogRef = useRef<HTMLDialogElement>(null);

  useEffect(() => {
    const el = dialogRef.current;
    // Restore focus to the launcher when the notice goes away (modal a11y).
    const opener = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    el?.showModal();
    return () => {
      el?.close();
      if (opener?.isConnected) opener.focus();
    };
  }, []);

  return (
    <dialog
      ref={dialogRef}
      aria-labelledby="license-lifecycle-title"
      aria-describedby="license-lifecycle-body"
      className="fixed inset-0 m-auto h-fit w-[min(460px,calc(100vw-32px))] rounded-[var(--radius-lg)] border border-[var(--glass-edge)] bg-[var(--surface-1)] p-[var(--space-5)] text-[var(--paper)] [box-shadow:0_24px_80px_rgba(0,0,0,0.45),inset_0_1px_0_var(--glass-highlight)] backdrop:bg-[rgba(0,0,0,0.48)] backdrop:[backdrop-filter:blur(10px)]"
      onCancel={(e) => {
        // Escape maps to the same soft-dismiss the secondary button performs
        // (this is an account-status notice, not a destructive confirmation),
        // rather than letting the native cancel event silently close it with
        // no state update on the React side.
        e.preventDefault();
        onSecondary();
      }}
    >
      <h2
        id="license-lifecycle-title"
        className="m-0 [font-family:var(--font-display)] text-[length:var(--size-xl)] leading-tight"
      >
        {dialog.title}
      </h2>
      <p
        id="license-lifecycle-body"
        className="mb-0 mt-[var(--space-3)] text-[length:var(--size-sm)] leading-relaxed text-[var(--muted)]"
      >
        {dialog.body}
      </p>
      <div className="mt-[var(--space-5)] flex flex-wrap justify-end gap-2">
        <button type="button" className={cx(ui.button, ui.buttonGhost)} onClick={onSecondary}>
          {dialog.secondaryAction}
        </button>
        <button type="button" className={ui.button} onClick={onPrimary}>
          {dialog.primaryAction}
        </button>
      </div>
    </dialog>
  );
}
