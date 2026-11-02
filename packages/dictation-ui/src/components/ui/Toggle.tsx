// Toggle - the brand switch control. Behaviour, ARIA and keys come from the owner's headless
// `useToggle`; this file keeps only the host app two visual grammars: the Apple-style filled track
// on macOS/iOS/Linux, and the WinUI toggle on Windows (outlined track, small solid thumb, ember
// fill when on).

import { useToggle } from "@rightkit/react-shell/headless";
import { getPlatform } from "../../hooks/usePlatform.js";

interface ToggleProps {
  checked: boolean;
  onChange: (next: boolean) => void;
  disabled?: boolean;
  ariaLabel?: string;
}

export default function Toggle({ checked, onChange, disabled, ariaLabel }: ToggleProps) {
  const win = getPlatform() === "windows";
  const switchProps = useToggle({ checked, onChange, label: ariaLabel ?? "", disabled: disabled === true });
  return (
    <button
      type="button"
      {...switchProps}
      aria-label={ariaLabel}
      className="group relative inline-flex min-h-11 min-w-11 cursor-pointer items-center justify-center gap-[var(--space-2)] border-0 bg-transparent p-0 focus-visible:outline-none aria-disabled:cursor-not-allowed"
    >
      <span
        aria-hidden="true"
        className={
          win
            ? // WinUI toggle: 40x20 transparent track with a 1px outline and a
              // 12px solid thumb; on = ember fill, white thumb, no outline.
              "relative h-[20px] w-10 rounded-full bg-transparent transition-[background-color,box-shadow] duration-150 ease-[var(--ease-out-strong)] [box-shadow:inset_0_0_0_1px_var(--muted)] after:absolute after:left-1 after:top-1 after:h-3 after:w-3 after:rounded-full after:bg-[var(--muted)] after:transition-[transform,background-color] after:duration-150 after:ease-[var(--ease-out-strong)] after:content-[''] group-aria-checked:bg-[var(--accent)] group-aria-checked:[box-shadow:none] group-aria-checked:after:translate-x-5 group-aria-checked:after:bg-white group-focus-visible:[box-shadow:var(--focus-ring)] group-aria-disabled:opacity-50"
            : "relative h-[18px] w-8 rounded-full bg-[var(--surface-3)] transition-[background-color,box-shadow] duration-150 ease-[var(--ease-out-strong)] after:absolute after:left-0.5 after:top-0.5 after:h-3.5 after:w-3.5 after:rounded-full after:bg-[var(--paper)] after:transition-transform after:duration-150 after:ease-[var(--ease-out-strong)] after:content-[''] group-aria-checked:bg-[var(--accent)] group-aria-checked:after:translate-x-3.5 group-focus-visible:[box-shadow:var(--focus-ring)] group-aria-disabled:opacity-50"
        }
      />
    </button>
  );
}
