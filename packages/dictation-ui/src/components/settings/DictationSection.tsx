import type { ReactNode } from "react";
import { cx, ui } from "../../lib/ui";

// Shared grammar for the Dictation page tabs (Vocabulary | Snippets | Text
// replacements): the add row, one quiet note, then ONE surface of hairline rows.
// The add form and the list rows share `dictationColumns` so inputs, rows and
// buttons line up on the same column edges and the same right edge.

// term / sounds-like | replacement / trailing action. The last column is the
// fixed width of the Add button so the remove buttons sit on its right edge.
export const dictationColumns =
  "grid grid-cols-[minmax(0,1fr)_minmax(0,1.3fr)_72px] items-center gap-x-[var(--space-2)]";

// The add form uses the same columns, but stacks on a very narrow window.
export const dictationFormColumns =
  "grid grid-cols-[minmax(0,1fr)_minmax(0,1.3fr)_72px] items-center gap-[var(--space-2)] @max-[340px]:grid-cols-1";

// Equal-height controls for the add row (input + button are the same 36px).
export const dictationInput = cx(
  ui.input,
  "!h-9 !min-h-9 w-full !py-0 !bg-[var(--hr-ink)] text-[length:var(--size-base)]",
);
// Ember only while the form is valid; disabled is a neutral surface.
export const dictationAddButton = cx(
  ui.button,
  "!h-9 !min-h-9 w-full !px-0 disabled:!bg-[var(--surface-3)] disabled:!text-[var(--muted-2)] disabled:!opacity-100",
);

// Quiet row-level remove button: 26px cell, danger only on hover.
export const dictationRowRemove =
  "grid h-[26px] w-[26px] shrink-0 place-items-center justify-self-end rounded-[6px] text-[var(--muted)] transition-colors hover:bg-[var(--surface-hover)] hover:text-[var(--danger)] focus-visible:outline-none focus-visible:[box-shadow:var(--focus-ring)]";

export const dictationRow = cx(dictationColumns, "min-h-[40px] px-[var(--space-3)] py-[6px]");

export const dictationEmpty =
  "px-[var(--space-3)] py-[var(--space-3)] text-[length:var(--size-sm)] text-[var(--muted)]";

// One quiet, one-line fact under a form (limits, scope). Same inset as the rows.
export const dictationHint =
  "m-0 px-[2px] text-[length:var(--size-xs)] leading-[1.4] text-[var(--muted)]";

// One tab body: add row + note on the page, then the list as a single surface.
export function DictationPanel({
  form,
  note,
  children,
}: {
  form: ReactNode;
  note: ReactNode;
  children: ReactNode;
}) {
  return (
    <div className="@container flex min-w-0 flex-col gap-[var(--space-2)]">
      {form}
      <p className={dictationHint}>{note}</p>
      <div className={cx(ui.surface, "mt-[var(--space-2)] overflow-hidden")}>
        <div className="divide-y divide-[color-mix(in_srgb,var(--line),transparent_22%)]">
          {children}
        </div>
      </div>
    </div>
  );
}
