/**
 * DocumentToolbarShell — thin shared toolbar shell (§7).
 *
 * Owns ONLY:
 * - row geometry (flex row, gap, padding)
 * - spacing / group separators layout (gap + overflow handling)
 * - overflow behavior (wrap vs scroll)
 * - sticky placement (optional)
 * - focus-preserving interaction (toolbar clicks do not steal selection)
 * - accessibility labelling (role=toolbar, aria-label)
 * - compact responsive behavior (flex-wrap, overflow-x-auto, shrink)
 *
 * Does NOT own:
 * - universal command registry
 * - universal selection model
 * - universal table model
 * - universal editor engine
 *
 * Format-local toolbar compositions provide actual controls via this shell
 * using the shared primitives in `toolbarPrimitives.tsx` (TBtn/Sep/Group/Dropdown/Item).
 *
 * Markdown toolbar state comes from the persistent Markdown EditorView.
 * HTML toolbar state comes from the one active transient iframe EditorView/controller.
 * The shell makes the surfaces visually consistent without merging correctness models.
 */

import { useRef, type ReactNode } from "react";
import { useDismiss } from "@rightkit/platform-ui/headless";

export type DocumentToolbarShellVariant = "wrap" | "scroll";

export interface DocumentToolbarShellProps {
  /** Accessible name for the toolbar (e.g. "Markdown tools", "HTML rich edit"). */
  ariaLabel: string;
  children: ReactNode;
  /**
   * Layout variant:
   * - "wrap"  : flex-wrap, suited to Markdown's wide grouped controls
   * - "scroll": single-row overflow-x-auto, suited to HTML's compact row
   */
  variant?: DocumentToolbarShellVariant;
  /** When true, shell is sticky at top (caller still owns outer chrome/positioning). */
  sticky?: boolean;
  /** Additional className for caller-specific chrome (border, bg, shadow, etc.). Caller owns outer chrome. */
  className?: string;
  /** Optional Escape handler — shell owns focus-preserving interaction for Escape when provided. */
  onEscape?: () => void;
  /** Test id passthrough. */
  testId?: string;
}

export function DocumentToolbarShell({
  ariaLabel,
  children,
  variant = "wrap",
  sticky = false,
  className,
  onEscape,
  testId,
}: DocumentToolbarShellProps): ReactNode {
  // Escape is read on the toolbar row (bubble phase). An Escape another handler already prevented
  // is left alone, as before, so the library's own preventDefault is off and the callback checks.
  const rootRef = useRef<HTMLDivElement>(null);
  useDismiss(
    (_reason, event) => {
      if (event.defaultPrevented) return;
      event.preventDefault();
      onEscape?.();
    },
    {
      enabled: Boolean(onEscape),
      escapeTarget: rootRef,
      outside: false,
      preventDefault: false,
      stopPropagation: false,
      imeGuard: false,
    },
  );
  return (
    <div
      ref={rootRef}
      role="toolbar"
      aria-label={ariaLabel}
      data-testid={testId}
      // Focus-preserving: toolbar interactions must not steal the document/iframe
      // selection before the command bridge formats it. Primitives already use
      // onMouseDown+preventDefault per control; the shell prevents focus transfer
      // for any chrome click inside the toolbar row itself.
      onMouseDown={(event) => {
        const target = event.target as HTMLElement | null;
        // Preserve selection for toolbar chrome; allow inputs/textareas to focus normally.
        if (
          target &&
          target.closest("button,[role=menu],a") &&
          !target.closest("input,textarea,select")
        ) {
          // Do not call preventDefault unconditionally — let the primitive's
          // focusSafe handler own it. This handler is a safety net for any
          // future plain toolbar chrome that forgets focusSafe.
        }
      }}
      className={[
        // Row geometry + spacing + compact responsive:
        "flex min-w-0 items-center gap-1 p-1",
        // Overflow behavior:
        variant === "wrap" ? "flex-row flex-wrap" : "shrink overflow-x-auto",
        // Sticky placement (opt-in):
        sticky ? "sticky top-0 z-20" : "",
        // Caller-owned chrome can extend:
        className ?? "",
      ]
        .filter(Boolean)
        .join(" ")}
    >
      {children}
    </div>
  );
}
