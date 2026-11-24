// Settings whose stored form differs from the one control that shows them: legacy values mapped on read, and a choice
// that folds two stored settings (an on/off flag and a key) into "off or one key". Pure; imports nothing.

/** `value` mapped through `legacy` (`{ oldValue: newValue }`); a value not in the map comes back unchanged. */
export function mapLegacyValue<T extends string>(value: string, legacy: Readonly<Record<string, T>>): string {
  return Object.prototype.hasOwnProperty.call(legacy, value) ? legacy[value]! : value;
}

/** The two stored settings behind a push-to-talk style choice. */
export interface FlagKeyChoice {
  /** The flag is on. */
  enabled: boolean;
  /** The stored key; empty or unset falls back to `fallbackKey`. */
  key: string | null | undefined;
  /** Key shown when none is stored. */
  fallbackKey: string;
  /** The choice value for "off". Default `"off"`. */
  offValue?: string;
}

/** The single choice to show: `offValue` while the flag is off, else the stored key (or `fallbackKey` when empty). */
export function flagKeyChoiceValue(choice: FlagKeyChoice): string {
  return choice.enabled ? choice.key || choice.fallbackKey : (choice.offValue ?? "off");
}

export interface WriteFlagKeyChoiceOptions {
  /** The new choice from the control. */
  value: string;
  /** Whether the flag is currently on. */
  enabled: boolean;
  /** The settings write (each call awaited in order). */
  patch(key: string, value: unknown): Promise<unknown>;
  /** Setting that holds the flag. */
  flagSetting: string;
  /** Setting that holds the key. */
  keySetting: string;
  /** The choice value for "off". Default `"off"`. */
  offValue?: string;
}

/**
 * Writes the choice. Off sets the flag false and leaves the key. A key is written first, then the flag is set back on
 * only if it was off, so the key is never lost and the flag never turns on without one.
 */
export async function writeFlagKeyChoice(options: WriteFlagKeyChoiceOptions): Promise<void> {
  const { value, enabled, patch, flagSetting, keySetting, offValue = "off" } = options;
  if (value === offValue) {
    await patch(flagSetting, false);
    return;
  }
  await patch(keySetting, value);
  if (!enabled) await patch(flagSetting, true);
}

/** `selection` with `key` set to `checked`; a copy, the input is not changed. */
export function withSelection<K extends string>(selection: Readonly<Partial<Record<K, boolean>>>, key: K, checked: boolean): Partial<Record<K, boolean>> {
  return { ...selection, [key]: checked };
}
