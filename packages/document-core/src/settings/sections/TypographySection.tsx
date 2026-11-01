import { SegmentedControl } from "@rightkit/react-shell/react";
import { SettingsGroup } from "@rightkit/settings-ui";
import { FONT_SIZES } from "../fonts.js";
import { useSettings } from "../settings.js";


export function TypographySection() {
  const { settings, update, reset } = useSettings();

  return (
    <>
      <SettingsGroup title="Size">
        <SegmentedControl
          label="Size"
          options={FONT_SIZES.map((s) => ({
            value: String(s.px),
            label: s.label,
            title: `${s.label} reading size (${s.px}px)`,
          }))}
          value={String(settings.fontSizePx)}
          onChange={(v) => update({ fontSizePx: Number(v) })}
        />
      </SettingsGroup>

      <SettingsGroup title="Line height">
        <input
          type="range"
          aria-label="Reading line height"
          min={1.3}
          max={2.1}
          step={0.05}
          value={settings.lineHeight}
          // A drag fires onChange on every pixel of movement; the settings store applies each change
          // optimistically, serialises the native commits, and applyTokens skips unchanged variables.
          onChange={(e) => update({ lineHeight: Number(e.target.value) })}
          className="w-full accent-[var(--copper)]"
        />
        <div className="mt-1 font-mono text-xs text-muted-2">{settings.lineHeight.toFixed(2)}</div>
      </SettingsGroup>

      <SettingsGroup title="Contrast">
        <SegmentedControl
          label="Contrast"
          options={[
            { value: "normal", label: "normal", title: "Normal reading contrast" },
            { value: "high", label: "high", title: "High reading contrast" },
          ]}
          value={settings.contrast}
          onChange={(v) => update({ contrast: v })}
        />
      </SettingsGroup>

      <button
        type="button"
        onClick={reset}
        className="vr-press vr-focus rounded px-2 py-1.5 font-sans text-xs text-muted-2 hover:bg-panel-2 hover:text-paper"
      >
        Reset to defaults
      </button>
    </>
  );
}
