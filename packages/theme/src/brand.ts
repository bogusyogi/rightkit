import { ensureContrast, isHex, luminance, mix, parseHex, readableOn, toHex } from "./color.js";

export type Mode = "light" | "dark";

/**
 * Brand file (`brand.json`). Superset of GenRight's approved file: every GenRight `colors` key keeps its meaning.
 *
 * - `colors.brand`           logo / decorative accent
 * - `colors["brand-action"]` accent for controls on light surfaces; falls back to `brand`
 * - `colors["brand-ink"]`    readable accent text on light surfaces; falls back to the action colour
 * - `colors["brand-soft"]`, `["brand-border"]`, `["brand-action-hover"]` carried through as CSS variables
 * - `dark`                   optional explicit dark values for any `brand-*` key
 * - `tokens`                 optional overrides of the suite colours per mode (surfaces, ink, status), emitted as `--rk-<name>`
 * - `wordmark`               the product name as written in the wordmark (e.g. "CutRight"); informational, never emitted as CSS.
 *                            A font-family list here is ignored, and `applyBrand` warns (the font goes in `wordmarkFont`)
 * - `wordmarkFont`           CSS font-family stack for the wordmark, emitted as `--rk-wordmark-font` (e.g. `"Tanker", sans-serif`)
 */
export interface BrandFile {
  name: string;
  code?: string;
  status?: string;
  colors: Record<string, string>;
  dark?: Record<string, string>;
  tokens?: { light?: Record<string, string>; dark?: Record<string, string> };
  wordmark?: string;
  wordmarkFont?: string;
  logo?: { master?: string; tileBackground?: string; geometry?: string };
  usage?: Record<string, string>;
}

/** The flat suite colours defined by `@rightkit/react-shell/shell.css`, per mode. */
export const SUITE_DEFAULTS: Record<Mode, Record<string, string>> = {
  light: {
    chrome: "#EEE9DF", plane: "#FFFCF5", elevated: "#FFFFFF", panel: "#FFFFFF", sunk: "#F6F1E8",
    ink: "#231F1A", "ink-2": "#6B655C", "ink-3": "#8A8378",
    ok: "#1E7F3C", warn: "#B25000", bad: "#C4001A",
    // Status aliases and the video letterbox surface (--rk-stage: footage stays near-black in both modes).
    stage: "#0B0C0D",
    success: "#1E7F3C", "success-soft": "rgba(30,127,60,.14)", "success-border": "rgba(30,127,60,.3)",
    warning: "#B25000", "warning-bg": "rgba(178,80,0,.14)",
    star: "#B8860B",
  },
  dark: {
    chrome: "#0F0F10", plane: "#17171A", elevated: "#1F1F23", panel: "#1F1F23", sunk: "#1C1C1F",
    ink: "#F2F0EC", "ink-2": "#969390", "ink-3": "#6E6B68",
    ok: "#4CD07D", warn: "#FFB340", bad: "#FF6961",
    stage: "#0B0C0D",
    success: "#16A766", "success-soft": "rgba(47,125,79,.12)", "success-border": "rgba(47,125,79,.3)",
    warning: "#FFAD47", "warning-bg": "rgba(255,173,71,.16)",
    star: "#E0B740",
  },
};

/**
 * The suite motion scale (not colours, so not per mode): `--rk-dur-1` micro feedback, `--rk-ease-out` entrances,
 * `--rk-ease-spring` overshoot. `checkSuiteDefaults` keeps these in step with `shell.css`.
 */
export const SUITE_MOTION: Readonly<Record<string, string>> = {
  "--rk-dur-1": "120ms",
  "--rk-ease-out": "cubic-bezier(0.16, 1, 0.3, 1)",
  "--rk-ease-spring": "cubic-bezier(0.34, 1.56, 0.64, 1)",
};

export const OVERRIDABLE_TOKENS = [
  "chrome", "plane", "elevated", "panel", "sunk", "ink", "ink-2", "ink-3", "ok", "warn", "bad",
  "ok-dot", "warn-dot", "line", "line-strong", "hairline", "accent-ink",
  "stage", "success", "success-soft", "success-border", "warning", "warning-bg", "star",
] as const;

const BRAND_KEY = /^brand(?:-[a-z]+)*$/;

export class BrandError extends Error {}

const isObject = (v: unknown): v is Record<string, unknown> => typeof v === "object" && v !== null && !Array.isArray(v);

/** Validate untrusted JSON into a BrandFile. Throws BrandError naming the offending path. */
export function parseBrand(input: unknown): BrandFile {
  if (!isObject(input)) throw new BrandError("brand file must be a JSON object");
  if (typeof input.name !== "string" || !input.name.trim()) throw new BrandError("name: required string");
  const colors = input.colors;
  if (!isObject(colors)) throw new BrandError("colors: required object");
  for (const [k, v] of Object.entries(colors)) {
    if (!BRAND_KEY.test(k)) throw new BrandError(`colors.${k}: names must match brand(-word)*`);
    if (!isHex(v)) throw new BrandError(`colors.${k}: expected #RRGGBB, got ${JSON.stringify(v)}`);
  }
  if (!("brand" in colors)) throw new BrandError("colors.brand: required");
  if (input.dark !== undefined) {
    if (!isObject(input.dark)) throw new BrandError("dark: expected object");
    for (const [k, v] of Object.entries(input.dark)) {
      if (!BRAND_KEY.test(k) || !isHex(v)) throw new BrandError(`dark.${k}: expected a brand-* name with #RRGGBB`);
    }
  }
  if (input.tokens !== undefined) {
    if (!isObject(input.tokens)) throw new BrandError("tokens: expected object");
    for (const [mode, map] of Object.entries(input.tokens)) {
      if (mode !== "light" && mode !== "dark") throw new BrandError(`tokens.${mode}: only light and dark are allowed`);
      if (!isObject(map)) throw new BrandError(`tokens.${mode}: expected object`);
      for (const [k, v] of Object.entries(map)) {
        if (!(OVERRIDABLE_TOKENS as readonly string[]).includes(k)) throw new BrandError(`tokens.${mode}.${k}: unknown name`);
        if (typeof v !== "string" || !(isHex(v) || /^rgba?\([^)]+\)$/i.test(v))) throw new BrandError(`tokens.${mode}.${k}: expected a hex or rgb() colour`);
      }
    }
  }
  if (input.wordmark !== undefined && typeof input.wordmark !== "string") throw new BrandError("wordmark: expected a string");
  if (input.wordmarkFont !== undefined && (typeof input.wordmarkFont !== "string" || /[;{}]/.test(input.wordmarkFont))) {
    throw new BrandError("wordmarkFont: expected a plain font-family string");
  }
  return input as unknown as BrandFile;
}

export interface ResolvedBrand {
  /** Flat suite colours that differ from shell.css defaults, per mode. */
  overrides: Record<Mode, Record<string, string>>;
  /** Full effective colour set per mode (defaults merged with overrides), used by the checks. */
  effective: Record<Mode, Record<string, string>>;
  /** Brand palette per mode. */
  palette: Record<Mode, Record<string, string>>;
}

/** The text surface (plane or elevated) on which accent text has the least contrast in `mode`. */
function worstSurface(brand: BrandFile, mode: Mode): string {
  const pick = (name: string) => {
    const v = brand.tokens?.[mode]?.[name] ?? SUITE_DEFAULTS[mode][name];
    return isHex(v) ? v : SUITE_DEFAULTS[mode][name];
  };
  const surfaces = [pick("plane"), pick("elevated")];
  const lum = (h: string) => luminance(h);
  return mode === "dark" ? surfaces.sort((a, b) => lum(b) - lum(a))[0] : surfaces.sort((a, b) => lum(a) - lum(b))[0];
}

/**
 * Resolve a brand file into per-mode values.
 * Light accent = brand-action ?? brand. Dark accent = dark["brand-action"] ?? dark.brand ?? the brand colour lifted to 4.5:1 on the
 * dark plane (a brand that already passes is kept as is). `accent-ink` is chosen per mode for the best legibility on the accent.
 */
export function resolveBrand(brand: BrandFile): ResolvedBrand {
  const c = brand.colors;
  const lightAccent = toHex(parseHex(c["brand-action"] ?? c.brand));
  const lightText = c["brand-ink"] ? toHex(parseHex(c["brand-ink"])) : ensureContrast(lightAccent, worstSurface(brand, "light"), 4.5);
  const darkPlane = worstSurface(brand, "dark");
  const darkBrand = brand.dark?.brand ?? ensureContrast(c.brand, darkPlane, 4.5);
  const darkAccent = brand.dark?.["brand-action"] ?? darkBrand;
  const darkText = brand.dark?.["brand-ink"] ?? ensureContrast(darkAccent, darkPlane, 4.5);

  const palette: Record<Mode, Record<string, string>> = {
    light: { ...c, brand: toHex(parseHex(c.brand)), "brand-action": lightAccent, "brand-ink": lightText },
    dark: {
      ...c,
      ...brand.dark,
      brand: toHex(parseHex(darkBrand)),
      "brand-action": toHex(parseHex(darkAccent)),
      "brand-ink": toHex(parseHex(darkText)),
      "brand-soft": brand.dark?.["brand-soft"] ?? mix(darkPlane, darkAccent, 0.18),
      "brand-border": brand.dark?.["brand-border"] ?? mix(darkPlane, darkAccent, 0.4),
    },
  };
  // The dark palette never inherits the light-only hover/soft colours by accident.
  if (!brand.dark?.["brand-action-hover"]) delete palette.dark["brand-action-hover"];

  const overrides: Record<Mode, Record<string, string>> = { light: {}, dark: {} };
  for (const mode of ["light", "dark"] as const) {
    overrides[mode] = { "accent-ink": readableOn(palette[mode]["brand-action"]), ...(brand.tokens?.[mode] ?? {}) };
  }
  const effective: Record<Mode, Record<string, string>> = {
    light: { ...SUITE_DEFAULTS.light, ...overrides.light },
    dark: { ...SUITE_DEFAULTS.dark, ...overrides.dark },
  };
  return { overrides, effective, palette };
}
