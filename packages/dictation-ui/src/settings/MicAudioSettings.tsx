import { memo } from "react";
import { CompactSettingRow, CompactSettingsGroup } from "../components/settings/CompactSettingsGroup.js";
import { SectionToggle as Toggle } from "./Toggle.js";
import { MicDevicePicker } from "./MicDevicePicker.js";
import type { PatchFn } from "../types/settings.js";

interface Props {
  loaded: boolean;
  inputDevice: string | undefined;
  soundEffectsOn: boolean;
  soundEffectsVolume: number;
  patch: PatchFn;
}

function MicAudioSettingsImpl({
  loaded,
  inputDevice,
  soundEffectsOn,
  soundEffectsVolume,
  patch,
}: Props) {
  return (
    <CompactSettingsGroup title="Audio">
      <CompactSettingRow
        label="Input device"
        settingId="settings.input-device"
        className="[&_select]:!min-h-8 [&_select]:!py-1 [&_select]:w-auto [&_select]:min-w-[200px] [&_select]:max-w-[300px] [&_select]:truncate"
      >
        <MicDevicePicker
          value={inputDevice}
          onChange={(value) => patch("input_device", value)}
          disabled={!loaded}
        />
      </CompactSettingRow>
      <CompactSettingRow label="Sound effects" settingId="settings.sound-effects">
        {soundEffectsOn ? (
          <input
            type="range"
            aria-label={`Sound effect volume (${soundEffectsVolume}%)`}
            min={0}
            max={100}
            value={soundEffectsVolume}
            onChange={(event) => patch("sound_effects_volume", Number(event.target.value))}
            disabled={!loaded}
            className="w-24 [accent-color:var(--accent)]"
          />
        ) : null}
        <Toggle
          checked={soundEffectsOn}
          disabled={!loaded}
          ariaLabel="Sound effects on state changes"
          onChange={(value) => patch("sound_effects_on", value)}
        />
      </CompactSettingRow>
    </CompactSettingsGroup>
  );
}

export const MicAudioSettings = memo(MicAudioSettingsImpl);
