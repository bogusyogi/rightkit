// Render-crash recovery configuration for the shared `@rightkit/react-shell` ErrorBoundary.
//
// The shared boundary owns catching, resetting and the recovery panel (its default fallback).
// the host app supplies only the panel label and the report: a content-free `render_crash` log line
// from the shared logs client (the root that crashed) and a scrubbed crash report through the same
// native command the window-error capture uses (`rightkit_crash_window_error`, scrubbed with the
// owner's `scrubWindowCrashText`). Before this existed, an uncaught render error in any window (hub, pill, popups) left a permanently
// blank surface; every `main.tsx` root now wraps its tree with
// `<ErrorBoundary {...renderCrashProps("<root>")}>`.
//
// `boundary` identifies which root crashed in the log line (e.g. "hub", "pill",
// "mini-clipboard", "hint", "splash") since all windows share one frontend bundle and one log
// stream.

import type { ErrorBoundaryProps } from "@rightkit/react-shell/react";
import { RIGHTKIT_CRASH_WINDOW_ERROR_COMMAND, scrubWindowCrashText } from "@rightkit/logs";
import type { ErrorInfo } from "react";
import { logs } from "../lib/logs.js";
import { invoke } from "../lib/tauri.js";
import { hostAppName } from "../appName.js";

export const RENDER_CRASH_LABEL = `${hostAppName()} hit an unexpected error in this window.`;

export function renderCrashProps(boundary: string): Pick<ErrorBoundaryProps, "label" | "onError"> {
  return {
    label: RENDER_CRASH_LABEL,
    onError: (error: Error, info: ErrorInfo) => {
      // Crash reporting must never raise a new error.
      try {
        logs.error("render_crash", { surface: boundary });
        void logs.flush().catch(() => undefined);
        const message = `render_crash boundary=${boundary} ${String(error?.message ?? error)}`;
        const stack = String(info?.componentStack ?? error?.stack ?? "");
        void invoke(RIGHTKIT_CRASH_WINDOW_ERROR_COMMAND, {
          report: {
            kind: "error",
            message: scrubWindowCrashText(message).slice(0, 1_000),
            stack: scrubWindowCrashText(stack).slice(0, 8_000),
            source: null,
          },
        }).catch(() => undefined);
      } catch {
        // never raise from the crash path
      }
    },
  };
}
