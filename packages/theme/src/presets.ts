import { isHex, luminance, mix } from "./color.js";
import { applyTokens, presetTokens, type StyleTarget, type TokenPreset } from "./tokens.js";

/** The subset of the Web Storage API the preset store uses. */
export interface PresetStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem(key: string): void;
}

export interface PresetStoreOptions {
  /** The app's preset catalogue. The lists stay per app; this package ships none. */
  presets: readonly TokenPreset[];
  /** Key that holds the chosen preset id. */
  storageKey: string;
  /** Older keys whose value is a preset id. The first one holding a known id is copied to `storageKey` and removed. */
  legacyKeys?: readonly string[];
  /** Storage to use. Default `localStorage` when available. `null` keeps nothing. Every access is guarded: storage that throws reads as empty. */
  storage?: PresetStorage | null;
}

export interface PresetStore {
  /** The stored preset id when it names a known preset, else `null`. Migrates a legacy key on first read. Never throws. */
  read(): string | null;
  /** Persist a preset id; `null` clears it. Never throws. */
  save(id: string | null): void;
  /** The preset with this id, if any. */
  find(id: string): TokenPreset | undefined;
  /** The token record for a preset and optional accent, with the derived `--rk-line-2`. `undefined` for an unknown id. */
  tokens(id: string, accentId?: string | null): Record<string, string> | undefined;
  /** Apply a preset's tokens to `root` and return the undo. An unknown id applies nothing. */
  apply(id: string, accentId?: string | null, root?: StyleTarget | null): () => void;
}

/**
 * The second hairline derived from a preset's line colour: the line nudged 6% toward the opposite pole
 * (white on a dark line, black on a light one). Returns `undefined` when the line is not hex.
 */
export function deriveLineSecond(line: string): string | undefined {
  if (!isHex(line)) return undefined;
  return mix(line, luminance(line) < 0.2 ? "#FFFFFF" : "#000000", 0.06);
}

function defaultStorage(): PresetStorage | null {
  try {
    return typeof localStorage === "undefined" ? null : localStorage;
  } catch {
    return null;
  }
}

/**
 * A preset-id store over `presetTokens`: persistence of the chosen preset with legacy-key migration, and the
 * token record per preset and accent. The derived `--rk-line-2` is added unless the preset sets it. The preset
 * list and the id keys stay with the app.
 */
export function createPresetStore(options: PresetStoreOptions): PresetStore {
  const { presets, storageKey, legacyKeys = [] } = options;
  const storage = options.storage === undefined ? defaultStorage() : options.storage;
  const known = (value: string | null): value is string => value !== null && presets.some((p) => p.id === value);
  const get = (key: string): string | null => {
    try {
      return storage?.getItem(key) ?? null;
    } catch {
      return null;
    }
  };
  const find = (id: string) => presets.find((p) => p.id === id);
  const tokensFor = (id: string, accentId?: string | null): Record<string, string> | undefined => {
    const preset = find(id);
    if (!preset) return undefined;
    const tokens = presetTokens(preset, accentId);
    const line = tokens["--rk-line"] ?? tokens["--line"];
    const second = line === undefined ? undefined : deriveLineSecond(line);
    return second === undefined || tokens["--rk-line-2"] !== undefined ? tokens : { ...tokens, "--rk-line-2": second };
  };
  return {
    read() {
      const current = get(storageKey);
      if (known(current)) return current;
      for (const legacy of legacyKeys) {
        const value = get(legacy);
        if (!known(value)) continue;
        try {
          storage?.setItem(storageKey, value);
          storage?.removeItem(legacy);
        } catch {
          /* the id is still returned for this session */
        }
        return value;
      }
      return null;
    },
    save(id) {
      try {
        if (id === null) storage?.removeItem(storageKey);
        else storage?.setItem(storageKey, id);
      } catch {
        /* storage refused the write: the choice lasts for this session only */
      }
    },
    find,
    tokens: tokensFor,
    apply(id, accentId, root) {
      const tokens = tokensFor(id, accentId);
      if (!tokens) return () => {};
      return applyTokens(tokens, root);
    },
  };
}
