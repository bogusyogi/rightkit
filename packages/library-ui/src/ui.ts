import { cx } from "@rightkit/react-shell/react";

export const panelClass = "rounded-[8px] border border-[var(--line)] bg-[var(--panel-2)]";

export const subtlePanelClass =
  "rounded-[8px] border border-[var(--line)] bg-[color-mix(in_srgb,var(--panel-2)_72%,var(--ink))]";

export const buttonClass =
  "inline-flex h-9 shrink-0 items-center justify-center gap-2 rounded-[6px] border border-[var(--line)] bg-[var(--panel-2)] px-3 text-sm font-medium text-[var(--paper)] transition-colors hover:border-[var(--accent)] hover:text-[var(--paper)] focus:outline-none focus:ring-2 focus:ring-[var(--accent)]/35 disabled:cursor-not-allowed disabled:opacity-55";

export const primaryButtonClass =
  "inline-flex h-12 shrink-0 items-center justify-center gap-2 rounded-[8px] border border-[var(--accent)] bg-[var(--accent)] px-5 text-base font-semibold text-[var(--ink)] shadow-[0_0_0_2px_var(--ink)] transition-colors hover:bg-[var(--accent-hi)] focus:outline-none focus:ring-2 focus:ring-[var(--accent)]/45";

export const inputClass =
  "w-full rounded-[8px] border border-[var(--line)] bg-[var(--panel-2)] px-3 py-2 text-sm text-[var(--paper)] outline-none transition-colors placeholder:text-[var(--muted)] focus:border-[var(--accent)]";

export const mutedTextClass = "text-[var(--muted)]";

export const errorTextClass = "text-[var(--warn)]";

// Compact 7x7 icon action button (hover-revealed inside a `group`). Shared by the
// queue card cancel/remove (danger) and re-run (accent) controls.
const iconActionBase =
  "h-7 w-7 rounded-[6px] text-[var(--muted)] opacity-60 transition-[opacity,background-color,color,transform] duration-150 focus:outline-none active:scale-95 group-hover:opacity-100";

export const iconActionDangerClass = cx(
  iconActionBase,
  "hover:bg-[color-mix(in_srgb,var(--warn)_14%,var(--panel-2))] hover:text-[var(--warn)] focus:ring-2 focus:ring-[var(--warn)]/35",
);

export const iconActionAccentClass = cx(
  iconActionBase,
  "hover:bg-[color-mix(in_srgb,var(--panel-2)_70%,var(--accent))] hover:text-[var(--accent)] focus:ring-2 focus:ring-[var(--accent)]/35",
);
