import { cx } from "@rightkit/react-shell/react";
import { ArrowLeft } from "lucide-react";
import type { ReactNode } from "react";
import { buttonClass } from "../lib/ui.js";

export function DetailShell({ children }: { children: ReactNode }) {
  return (
    <main className="desktop-reader flex h-full min-h-0 flex-1 flex-col gap-2 p-3">{children}</main>
  );
}

export function BackButton({ onBack }: { onBack: () => void }) {
  return (
    <button type="button" className={buttonClass} onClick={onBack}>
      <ArrowLeft size={15} strokeWidth={1.9} />
      <span>Back</span>
    </button>
  );
}

export function TabButton({
  active,
  disabled,
  onClick,
  children,
}: {
  active: boolean;
  disabled?: boolean;
  onClick: () => void;
  children: ReactNode;
}) {
  return (
    <button
      type="button"
      className={cx(
        "h-8 rounded-[6px] px-3 font-[family-name:var(--mono)] text-xs transition-colors disabled:cursor-not-allowed disabled:opacity-45",
        active
          ? "bg-[var(--accent)] text-[var(--ink)] hover:text-[var(--ink)]"
          : "text-[var(--muted)] hover:text-[var(--paper)]",
      )}
      disabled={disabled}
      aria-pressed={active}
      onClick={onClick}
    >
      {children}
    </button>
  );
}
