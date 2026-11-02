import type { ErrorInfo, ReactNode } from "react";
import { ErrorBoundary as ShellErrorBoundary } from "@rightkit/react-shell/react";
import { logError } from "@rightkit/document-core/state/diagnostics";
import { hostAppName } from "../appName.js";

interface Props {
  children: ReactNode;
  label?: string;
  /** The shell boundary clears a shown error when this value changes. */
  resetKey?: string;
  /** Path of the document tab this boundary guards; enables the "Close tab" action. */
  closeTabPath?: string;
}

/** the host app crash screen over the shell's boundary (`resetKey`, reset, `onError`). */
export function ErrorBoundary({ children, label, resetKey, closeTabPath }: Props) {
  const name = label ?? hostAppName();
  const report = (error: Error, info: ErrorInfo) => {
    console.error(`${hostAppName()} render error`, { label, error, info });
    // Static label only — no message, stack or paths — so a crash actually reaches the local
    // diagnostics log instead of vanishing (this boundary previously only reached `console.error`,
    // invisible to anyone not watching devtools at the moment it happened).
    logError("react:boundary");
  };

  return (
    <ShellErrorBoundary
      label={name}
      resetKey={resetKey}
      onError={report}
      fallback={(_error, reset) => (
        <div className="flex h-full min-h-0 items-center justify-center bg-ink p-6">
          <div
            role="alert"
            className="max-w-md rounded border border-line bg-panel-2 p-5 text-sm text-muted shadow-xl"
          >
            <p className="mb-1 font-serif text-lg font-semibold text-paper">
              Something went wrong in {name}
            </p>
            <p className="mb-4">
              This view couldn&apos;t be displayed. Your unsaved edits are kept in memory and
              nothing was written to your file. Reload the view to try again, or close the tab (you
              will be asked about unsaved changes).
            </p>
            <button
              type="button"
              aria-label={`Reload ${name}`}
              onClick={reset}
              className="vr-press vr-focus rounded bg-copper/20 px-3 py-1.5 font-sans text-chrome-sm text-copper-hi hover:bg-copper/30"
            >
              Reload
            </button>
            {closeTabPath ? (
              <button
                type="button"
                aria-label="Close tab"
                onClick={() =>
                  window.dispatchEvent(
                    new CustomEvent("viewright:close-tab", { detail: { path: closeTabPath } }),
                  )
                }
                className="vr-press vr-focus ml-2 rounded px-3 py-1.5 font-sans text-chrome-sm text-muted hover:bg-ink/40 hover:text-paper"
              >
                Close tab
              </button>
            ) : null}
          </div>
        </div>
      )}
    >
      {children}
    </ShellErrorBoundary>
  );
}
