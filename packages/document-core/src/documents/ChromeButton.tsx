import type { ReactNode } from "react";

/** Shared bordered toolbar icon/text button for preview chromes (HTML document chrome + Live Preview).
 *  Press/focus affordances, an `active` (copper) state, a `disabled` state, and an optional extra
 *  `className`. One component so the two preview toolbars stay visually identical. */
export function ChromeButton({
  ariaLabel,
  title,
  disabled,
  active,
  onClick,
  className,
  children,
}: {
  ariaLabel: string;
  title?: string;
  disabled?: boolean;
  active?: boolean;
  onClick: () => void;
  className?: string;
  children: ReactNode;
}) {
  return (
    <button
      type="button"
      aria-label={ariaLabel}
      aria-pressed={active}
      title={title ?? ariaLabel}
      disabled={disabled}
      onClick={onClick}
      className={`vr-press vr-focus flex h-8 shrink-0 items-center justify-center rounded-lg border px-2 py-1 disabled:cursor-not-allowed disabled:opacity-40 disabled:hover:border-line disabled:hover:text-muted ${
        active
          ? "border-copper/60 bg-copper/20 text-copper-hi"
          : "border-line text-muted hover:border-copper hover:text-copper-hi"
      } ${className ?? ""}`}
    >
      {children}
    </button>
  );
}
