// Push-to-talk key resolution for the setup preview: which setting holds the key on each platform, its default,
// and which keyboard matcher applies. No host names: the setting keys, defaults and matchers are inputs.

/** Where the push-to-talk key is stored and what it falls back to. */
export interface ResolvePttKeyConfig {
  /** Reads a setting by key with a fallback, e.g. the host settings hook's `get`. */
  getSetting: (key: string, fallback: string) => unknown;
  /** Setting key (and default) used when the platform is macOS. */
  macos: { key: string; default: string };
  /** Setting key (and default) used on every other platform. */
  other: { key: string; default: string };
  /** The platform id that selects `macos`. Default `"macos"`. */
  macPlatform?: string;
}

/** The push-to-talk key for `platform`: the stored value, or the platform default when it is empty. */
export function resolvePttKey(platform: string, config: ResolvePttKeyConfig): string {
  const side = platform === (config.macPlatform ?? "macos") ? config.macos : config.other;
  return (config.getSetting(side.key, side.default) as string) || side.default;
}

/** Key matchers per platform: `(code, configuredKey) => boolean` over `KeyboardEvent.code`. */
export interface PlatformPttMatchers {
  macos: (code: string, configuredKey: string) => boolean;
  other: (code: string, configuredKey: string) => boolean;
  /** The platform id that selects `macos`. Default `"macos"`. */
  macPlatform?: string;
}

/** An `isPttKeyEvent` that dispatches to the platform's matcher (the host passes e.g. platform-ui's helpers). */
export function platformPttMatcher(
  platform: string,
  matchers: PlatformPttMatchers,
): (event: KeyboardEvent, pttKey: string) => boolean {
  const match = platform === (matchers.macPlatform ?? "macos") ? matchers.macos : matchers.other;
  return (event, key) => match(event.code, key);
}
