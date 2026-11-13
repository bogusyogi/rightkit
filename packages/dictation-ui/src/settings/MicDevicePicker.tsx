// MicDevicePicker - input-device dropdown backed by useMicDevices (which owns
// enumeration + unmount guard). Renders status text while enumerating / on
// error / when empty.

import { useEffect, useMemo } from "react";
import { useMicDevices, type MicDevice } from "../hooks/useMicDevices.js";
import { ui } from "../lib/ui.js";

interface MicDevicePickerProps {
  value: string | undefined;
  onChange: (next: string) => void;
  disabled?: boolean;
  refreshToken?: number;
  onResolvedLabel?: (label: string) => void;
}

interface SavedDeviceParts {
  id?: string;
  platformId?: string;
  name: string;
}

function savedDeviceParts(current: string | undefined): SavedDeviceParts | undefined {
  if (current?.startsWith("device:")) {
    try {
      const parsed = JSON.parse(current.slice(7)) as {
        platform_id?: unknown;
        name?: unknown;
      };
      if (typeof parsed.name !== "string") return undefined;
      return {
        platformId: typeof parsed.platform_id === "string" ? parsed.platform_id : undefined,
        name: parsed.name,
      };
    } catch {
      return undefined;
    }
  }
  if (!current?.startsWith("id:")) return undefined;
  const rest = current.slice(3);
  const separator = rest.indexOf(":");
  if (separator < 0) return undefined;
  return { id: rest.slice(0, separator), name: rest.slice(separator + 1) };
}

function deviceSettingValue(
  device: MicDevice,
  current: string | undefined,
  saved: SavedDeviceParts | undefined,
  isUniqueLegacyMatch: boolean,
): string {
  if (
    current &&
    ((saved?.platformId && saved.platformId === device.platform_id) ||
      (!saved?.platformId &&
        saved?.id === String(device.id) &&
        saved.name === device.name) ||
      (saved?.name === device.name && isUniqueLegacyMatch))
  ) {
    return current;
  }
  if (current && !saved && isUniqueLegacyMatch) {
    return current;
  }
  return `device:${JSON.stringify({ platform_id: device.platform_id ?? null, name: device.name })}`;
}

export function MicDevicePicker({
  value,
  onChange,
  disabled,
  refreshToken = 0,
  onResolvedLabel,
}: MicDevicePickerProps) {
  const { devices, loading, error, refresh } = useMicDevices(refreshToken);
  // Memoized: the saved-value JSON.parse and the per-device option build used
  // to run on every render (and the unique-name filter ran once per device,
  // O(n²)). `savedDevice` is parsed once and passed in — `deviceSettingValue`
  // must not re-parse per option.
  const savedDevice = useMemo(() => savedDeviceParts(value), [value]);
  const options = useMemo(
    () =>
      devices.map((device) => ({
        device,
        value: deviceSettingValue(
          device,
          value,
          savedDevice,
          Boolean(
            value &&
              (savedDevice?.name === device.name || value === device.name) &&
              devices.filter((candidate) => candidate.name === device.name).length === 1,
          ),
        ),
      })),
    [devices, value, savedDevice],
  );
  const selectedOption = options.find((option) => option.value === value);
  const defaultDevice = useMemo(() => devices.find((device) => device.default), [devices]);
  const usesSystemDefault = !value || value === "__default__" || value.toLowerCase() === "default";
  const defaultLabel = defaultDevice
    ? `System default (${defaultDevice.name})`
    : "System default microphone";
  const selectedLabel = selectedOption?.device.name ?? savedDevice?.name;

  useEffect(() => {
    if (!onResolvedLabel) return;
    if (usesSystemDefault) {
      onResolvedLabel(defaultLabel);
      return;
    }
    onResolvedLabel(selectedLabel ?? "Saved microphone");
  }, [defaultLabel, onResolvedLabel, selectedLabel, usesSystemDefault]);

  if (loading) return <span className={ui.muted}>Enumerating…</span>;
  if (error && devices.length === 0) return <span className={ui.dangerText}>error: {error}</span>;
  if (devices.length === 0) return <span className={ui.muted}>No input devices found</span>;

  return (
    <select
      className={ui.input}
      aria-label="Input device"
      value={usesSystemDefault ? "__default__" : value}
      onChange={(e) => onChange(e.target.value)}
      onFocus={refresh}
      disabled={disabled}
    >
      <option value="__default__">System default</option>
      {value && !usesSystemDefault && !selectedOption ? (
        <option value={value} disabled>
          {savedDevice?.name ?? "Saved microphone"} (unavailable)
        </option>
      ) : null}
      {options.map(({ device, value: optionValue }) => (
        <option key={device.platform_id ?? `${device.id}:${device.name}`} value={optionValue}>
          {device.name}
          {device.default ? " (default)" : ""}
        </option>
      ))}
    </select>
  );
}
