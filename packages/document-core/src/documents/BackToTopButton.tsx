import { ArrowUp } from "@phosphor-icons/react/dist/icons/ArrowUp";

interface BackToTopButtonProps {
  onClick: () => void;
  className?: string;
}

export function BackToTopButton({ onClick, className = "" }: BackToTopButtonProps) {
  return (
    <button
      type="button"
      aria-label="Back to top"
      title="Back to top"
      onClick={onClick}
      className={`vr-press vr-focus no-print fixed bottom-10 right-5 z-50 grid h-9 w-9 place-items-center rounded-full border border-line bg-panel-2/95 text-muted shadow-xl backdrop-blur transition-colors hover:border-copper hover:text-copper-hi ${className}`}
    >
      <ArrowUp size={16} weight="regular" aria-hidden />
    </button>
  );
}
