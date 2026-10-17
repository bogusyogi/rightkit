import type { Toast } from "../toast.js";

/**
 * Implicit ("newest") dedupe merges a repeat into the row already on screen. Actionable toasts are never merged: the
 * action callback belongs to the item that raised it, so a second "Deleted / Undo" must keep its own callback.
 * Internal to the package: not re-exported from react.tsx.
 */
export function isImplicitDuplicate(shown: Toast, incoming: Toast): boolean {
  return !shown.action && !incoming.action && shown.kind === incoming.kind && shown.message === incoming.message;
}
