// SegmentedControl - the settings-row segmented choice control.
// Selected segment uses neutral elevated chip across all platforms;
// ember is reserved for primary actions, switches, active tabs, and recording.

import { useSegmentedControl } from "@rightkit/react-shell/headless";
import { cx } from "../../lib/ui";

export interface SegmentedOption<T extends string> {
  value: T;
  label: string;
  title?: string;
}

const segmentClass =
  "rounded-[calc(var(--radius-sm)-2px)] px-[11px] py-[3px] text-[length:var(--size-xs)] font-medium transition-colors duration-150 ease-[var(--ease-out-strong)] focus-visible:outline-none focus-visible:[box-shadow:var(--focus-ring)] disabled:cursor-not-allowed disabled:opacity-55";

export default function SegmentedControl<T extends string>({
  label,
  value,
  options,
  disabled,
  onChange,
}: {
  // Accessible group name (e.g. "Show pill").
  label: string;
  value: T;
  options: ReadonlyArray<SegmentedOption<T>>;
  disabled?: boolean;
  onChange: (value: T) => void;
}) {
  // Group and option behaviour (aria-pressed buttons, tab stops) is the owner's headless control.
  const { groupProps, getOptionProps } = useSegmentedControl<T>({
    value,
    options: options.map((option) => ({ value: option.value })),
    onChange,
    label,
    disabled,
    mode: "toggle",
  });
  return (
    <span
      {...groupProps}
      className="inline-grid grid-flow-col gap-[3px] rounded-[var(--radius-sm)] border border-[color-mix(in_srgb,var(--line),transparent_30%)] bg-[var(--surface-1)] p-[3px]"
    >
      {options.map((option) => {
        const active = value === option.value;
        return (
          <button
            key={option.value}
            type="button"
            disabled={disabled}
            {...getOptionProps(option.value)}
            aria-label={`${label}: ${option.label}`}
            title={option.title ?? `${label}: ${option.label}`}
            className={cx(
              segmentClass,
              active
                ? "bg-[var(--surface-3)] font-semibold text-[var(--paper)] [box-shadow:0_1px_2px_rgba(0,0,0,.24),0_0_0_.5px_rgba(0,0,0,.08)]"
                : "text-[var(--muted)] hover:bg-[var(--surface-hover)] hover:text-[var(--paper)]",
            )}
          >
            {option.label}
          </button>
        );
      })}
    </span>
  );
}
