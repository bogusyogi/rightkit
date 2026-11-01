import type { ToastKind } from "@rightkit/react-shell";

/** The subset of RightKit's toast kinds a status line can classify into. */
export type StatusToastKind = Extract<ToastKind, "info" | "success" | "error">;

const STEADY_LABELS = new Set(["ready", "preview mode"]);

const FAILURE_KEYWORDS = ["failed", "unavailable", "error", "could not", "denied", "rejected"];

const SUCCESS_ALLOWLIST = [
  "session deleted",
  "settings saved",
  "session created",
  "configuring coderight",
];

/** Classifies a transient status-line message into a toast kind, or
 * `null` when the message shouldn't also surface as a toast (steady labels,
 * routine in-progress text, and anything not on the success allowlist). Kept
 * as a pure function so the classification rule is unit-testable without the
 * full useDaemonConnection hook. */
export function classifyStatusForToast(message: string): StatusToastKind | null {
  const trimmed = message.trim();
  if (!trimmed) return null;
  const lower = trimmed.toLowerCase();
  if (STEADY_LABELS.has(lower)) return null;
  if (FAILURE_KEYWORDS.some((kw) => lower.includes(kw))) return "error";
  if (SUCCESS_ALLOWLIST.some((kw) => lower.startsWith(kw))) return "success";
  return null;
}
