import { CheckCircle } from "@phosphor-icons/react/dist/icons/CheckCircle";
import { Info } from "@phosphor-icons/react/dist/icons/Info";
import { WarningCircle } from "@phosphor-icons/react/dist/icons/WarningCircle";
import { defaultToastBus, type ToastKind } from "@rightkit/platform-ui/headless";
import {
  ToastViewport as ShellToastViewport,
  type ToastRowContext,
} from "@rightkit/react-shell/react";

const KIND_ICON: Record<ToastKind, typeof Info> = {
  info: Info,
  success: CheckCircle,
  warning: WarningCircle,
  error: WarningCircle,
};

// the host app row markup, unchanged. The shell supplies the timers, bus subscription, stack cap
// and exit timing; `leaving` ignores a second dismiss during the exit animation, as before.
function renderToastRow({ toast, leaving, dismiss, role }: ToastRowContext) {
  const KindIcon = KIND_ICON[toast.kind];
  return (
    <div
      role={role}
      className={`vr-toast-row${leaving ? " vr-toast-exit" : ""} vr-capsule vr-over-content pointer-events-auto h-10 w-fit max-w-full gap-2.5 pl-4 pr-2 font-sans text-chrome-md font-medium text-paper`}
    >
      <KindIcon size={16} weight="regular" aria-hidden="true" className="shrink-0 text-copper" />
      <span className="min-w-0 flex-1 truncate">{toast.message}</span>
      {toast.action && (
        <button
          type="button"
          onClick={() => {
            if (leaving) return;
            toast.action?.run();
            dismiss();
          }}
          className="vr-press vr-focus flex h-6 shrink-0 items-center rounded-full px-2.5 text-copper hover:bg-ink/40 hover:text-paper"
        >
          {toast.action.label}
        </button>
      )}
      <button
        type="button"
        onClick={() => {
          if (!leaving) dismiss();
        }}
        aria-label="Dismiss notification"
        className="vr-press vr-focus flex h-6 w-6 shrink-0 items-center justify-center rounded-full text-muted-2 hover:bg-ink/40 hover:text-paper"
      >
        ✕
      </button>
    </div>
  );
}

// Single mount point for the toast bus. Centred bottom capsules; the live region announces new
// toasts to assistive tech (polite for info/success, the error rows carry role="alert").
export function ToastViewport() {
  return (
    <ShellToastViewport
      bus={defaultToastBus}
      className="no-print pointer-events-none fixed bottom-4 left-1/2 z-[95] flex max-h-[calc(100vh-2rem)] w-[min(24rem,calc(100vw-2rem))] -translate-x-1/2 flex-col items-center gap-2 overflow-y-auto"
      roles={{ error: "alert", info: null, success: null, warning: null }}
      renderRow={renderToastRow}
    />
  );
}
