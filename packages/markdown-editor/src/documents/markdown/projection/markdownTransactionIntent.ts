/**
 * Contract 14 CE-2A — transaction intent classification.
 *
 * Every doc-changing ProseMirror transaction that reaches the document core
 * carries a user-level intent. The intent decides the core history grouping:
 * one user action (a toolbar click, a paste, one table operation) lands as ONE
 * history entry, while typing/delete coalesce under their existing rules.
 *
 * Priority: explicit meta (set by command authors) > ProseMirror's `uiEvent`
 * annotation (input/delete/paste driven by the view) > step heuristics.
 */

import type { Transaction } from "prosemirror-state";
import { ReplaceStep } from "prosemirror-transform";
import type { MarkdownHistoryLabel } from "../core/types";

export type MarkdownTransactionIntent =
  | "typing"
  | "delete"
  | "paste"
  | "format"
  | "table-structural"
  | "other";

export const MARKDOWN_INTENT_META = "viewright:markdown-intent";

/** Stamp an explicit intent on a transaction before dispatching it. */
export function setMarkdownTransactionIntent<T extends Transaction>(
  transaction: T,
  intent: MarkdownTransactionIntent,
): T {
  return transaction.setMeta(MARKDOWN_INTENT_META, intent) as T;
}

/** How a classified intent maps onto the core commit/history contract. */
export function commitOptionsForIntent(intent: MarkdownTransactionIntent): {
  label: MarkdownHistoryLabel;
  coalesceKey?: string;
  isolateHistory: boolean;
} {
  switch (intent) {
    case "typing":
      return { label: "Typing", coalesceKey: "hybrid:typing", isolateHistory: false };
    case "delete":
      return { label: "Delete", coalesceKey: "hybrid:delete", isolateHistory: false };
    case "paste":
      // One paste is one history entry, never merged with surrounding typing.
      return { label: "Paste", isolateHistory: true };
    case "format":
      // Formatting toggles are discrete user actions.
      return { label: "Format", isolateHistory: true };
    case "table-structural":
      // One toolbar table operation = one bounded source patch + one entry.
      return { label: "Table", isolateHistory: true };
    case "other":
      return { label: "Edit", isolateHistory: false };
  }
}

function intentFromUserEvent(userEvent: string | undefined): MarkdownTransactionIntent | null {
  if (!userEvent) return null;
  if (userEvent.startsWith("input.type")) return "typing";
  if (userEvent.startsWith("delete")) return "delete";
  if (userEvent.startsWith("paste") || userEvent.startsWith("drop")) return "paste";
  return null;
}

function intentFromSteps(transaction: Transaction): MarkdownTransactionIntent {
  // Fallback heuristics for transactions without an explicit or view-level
  // annotation (programmatic edits, mark toggles from adapters).
  if (!transaction.docChanged) return "other";
  let onlyTextInsert = true;
  let onlyDeletion = true;
  for (const step of transaction.steps) {
    if (!(step instanceof ReplaceStep)) {
      onlyTextInsert = false;
      onlyDeletion = false;
      break;
    }
    const slice = step.slice;
    if (slice.size === 0) {
      onlyTextInsert = false;
    } else if (slice.content.childCount === 1 && slice.content.firstChild?.isText) {
      onlyDeletion = false;
    } else {
      onlyTextInsert = false;
      onlyDeletion = false;
      break;
    }
  }
  if (onlyTextInsert && !onlyDeletion) return "typing";
  if (onlyDeletion && !onlyTextInsert) return "delete";
  return "other";
}

/** Classify a transaction into a user-level intent. */
export function classifyMarkdownTransaction(transaction: Transaction): MarkdownTransactionIntent {
  const explicit = transaction.getMeta(MARKDOWN_INTENT_META) as
    | MarkdownTransactionIntent
    | undefined;
  if (explicit) return explicit;
  const fromUserEvent = intentFromUserEvent(transaction.getMeta("uiEvent") as string | undefined);
  if (fromUserEvent) return fromUserEvent;
  return intentFromSteps(transaction);
}
