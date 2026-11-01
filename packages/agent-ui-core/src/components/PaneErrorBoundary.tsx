import { Button, ErrorBoundary as ShellErrorBoundary } from "@rightkit/react-shell/react";
import React, { useCallback, useState } from "react";
import Icon from "./Icon.js";
import { hostAppName } from "../appName.js";

/**
 * Per-pane error boundary. A crash inside one workbench surface shows a compact fallback scoped to
 * that pane instead of unmounting the whole app — the root ErrorBoundary in main.tsx stays as the
 * last resort. Catching and reset are RightKit's `ErrorBoundary` (@rightkit/react-shell); this
 * adapter supplies the pane fallback and remounts children on reload.
 */
export default function PaneErrorBoundary({
  name,
  children,
}: {
  name: string;
  children: React.ReactNode;
}) {
  const [remountKey, setRemountKey] = useState(0);
  const onError = useCallback(
    (error: Error, info: React.ErrorInfo) => {
      console.error(`${hostAppName()} pane "${name}" crashed:`, error, info.componentStack);
    },
    [name],
  );

  return (
    <ShellErrorBoundary
      onError={onError}
      fallback={(error, reset) => (
        <div
          role="alert"
          aria-live="assertive"
          className="flex flex-1 min-h-0 flex-col items-center justify-center gap-3 p-8 text-center"
        >
          <div className="flex items-center gap-2 text-danger">
            <Icon name="flag" size={16} />
            <strong className="text-[13px] font-semibold">{name} hit an error</strong>
          </div>
          <p className="max-w-sm text-[12px] leading-[1.5] text-text-2">{error.message}</p>
          <div className="flex gap-2">
            <Button
              variant="primary"
              size="sm"
              onClick={() => {
                setRemountKey((key) => key + 1);
                reset();
              }}
              aria-label={`Reload ${name}`}
            >
              Reload pane
            </Button>
            <Button
              variant="secondary"
              size="sm"
              onClick={() => {
                const text = `Pane: ${name}\n${error.message}\n${error.stack ?? ""}`;
                void navigator.clipboard?.writeText(text).catch(() => {});
              }}
            >
              Copy details
            </Button>
          </div>
        </div>
      )}
    >
      <React.Fragment key={remountKey}>{children}</React.Fragment>
    </ShellErrorBoundary>
  );
}
