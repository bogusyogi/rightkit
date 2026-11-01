import { permissionRulePresets } from "../../types";
import type { PermissionRuleEffect } from "@rightkit/agent-ui-core/lib/types";

export default function PermissionPresetGrid({
  disabled = false,
  keyPrefix = "",
  onSelect,
}: {
  disabled?: boolean;
  keyPrefix?: string;
  onSelect: (preset: { pattern: string; effect: PermissionRuleEffect }) => void;
}) {
  const groupLabel = keyPrefix
    ? "Inherited permission rule presets"
    : "Session permission rule presets";

  return (
    <fieldset
      className="m-0 grid max-w-[760px] grid-cols-1 gap-3 border-0 p-0 xl:grid-cols-2"
      aria-label={groupLabel}
    >
      {permissionRulePresets.map((preset) => (
        <button
          type="button"
          className="flex items-center justify-between gap-3 min-h-[48px] px-4 py-3 rounded-xl border border-line bg-panel text-left cursor-pointer hover:bg-panel-2 disabled:opacity-40"
          aria-label={`${preset.label}: ${preset.pattern}; ${preset.effect}`}
          disabled={disabled}
          key={`${keyPrefix}${preset.pattern}:${preset.effect}`}
          onClick={() => onSelect(preset)}
        >
          <span className="flex flex-col gap-1 min-w-0">
            <strong className="text-[13px] text-text truncate">{preset.label}</strong>
            <code className="text-[11px] text-text-3 truncate">{preset.pattern}</code>
          </span>
          <code className="text-[11px] text-accent-2">{preset.effect}</code>
        </button>
      ))}
    </fieldset>
  );
}
