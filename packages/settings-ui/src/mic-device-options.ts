// Saved-microphone matching for a picker: parses the persisted device value, builds one option per device
// whose value re-identifies it (by platform id, else by legacy id plus name, else by a unique name), and
// resolves which option the saved value selects. Pure; imports nothing. The persisted formats are kept:
// `device:{"platform_id":..,"name":..}` and the legacy `id:<n>:<name>`.
// Moved from HeardRight `components/settings/speech/MicDevicePicker.tsx`.

export interface MicDeviceLike {
  id: number;
  name: string;
  default: boolean;
  platform_id?: string | null;
}

export interface SavedMicDeviceParts {
  id?: string;
  platformId?: string;
  name: string;
}

export function parseSavedMicDevice(current: string | undefined): SavedMicDeviceParts | undefined {
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

/** The value to persist when this device is chosen (or the saved value itself when it already names the device). */
export function micDeviceSettingValue(
  device: MicDeviceLike,
  current: string | undefined,
  saved: SavedMicDeviceParts | undefined,
  isUniqueLegacyMatch: boolean,
): string {
  if (
    current &&
    ((saved?.platformId && saved.platformId === device.platform_id) ||
      (!saved?.platformId && saved?.id === String(device.id) && saved.name === device.name) ||
      (saved?.name === device.name && isUniqueLegacyMatch))
  ) {
    return current;
  }
  if (current && !saved && isUniqueLegacyMatch) {
    return current;
  }
  return `device:${JSON.stringify({ platform_id: device.platform_id ?? null, name: device.name })}`;
}

export interface MicDeviceOption<D extends MicDeviceLike> {
  device: D;
  value: string;
}

export interface MicDeviceSelection<D extends MicDeviceLike> {
  saved: SavedMicDeviceParts | undefined;
  options: MicDeviceOption<D>[];
  selected: MicDeviceOption<D> | undefined;
  defaultDevice: D | undefined;
  usesSystemDefault: boolean;
  /** The name of the selected or saved device, when one is known. */
  selectedName: string | undefined;
}

/** The sentinel option value for "use the system default". */
export const SYSTEM_DEFAULT_MIC = "__default__";

export function resolveMicSelection<D extends MicDeviceLike>(
  devices: readonly D[],
  value: string | undefined,
): MicDeviceSelection<D> {
  const saved = parseSavedMicDevice(value);
  const options = devices.map((device) => ({
    device,
    value: micDeviceSettingValue(
      device,
      value,
      saved,
      Boolean(
        value &&
          (saved?.name === device.name || value === device.name) &&
          devices.filter((candidate) => candidate.name === device.name).length === 1,
      ),
    ),
  }));
  return {
    saved,
    options,
    selected: options.find((option) => option.value === value),
    defaultDevice: devices.find((device) => device.default),
    usesSystemDefault: !value || value === SYSTEM_DEFAULT_MIC || value.toLowerCase() === "default",
    selectedName: options.find((option) => option.value === value)?.device.name ?? saved?.name,
  };
}

/** Words for `micSelectionLabel`. */
export interface MicSelectionLabels {
  /** The system-default line; `deviceName` is the default device's name when one is known. */
  systemDefault(deviceName: string | undefined): string;
  /** Shown when the saved device has no known name. */
  savedFallback: string;
}

/**
 * The one line that says which microphone is in use: the system-default line while the value selects the default, else the
 * selected or saved device's name, else `savedFallback`. The host shows it, or reports it to a parent.
 */
export function micSelectionLabel<D extends MicDeviceLike>(selection: MicDeviceSelection<D>, labels: MicSelectionLabels): string {
  return selection.usesSystemDefault ? labels.systemDefault(selection.defaultDevice?.name) : (selection.selectedName ?? labels.savedFallback);
}
