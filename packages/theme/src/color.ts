/** Colour maths shared by the generator, the checks and the runtime. Hex in, hex out; alpha is ignored for contrast. */

export interface Rgb {
  r: number;
  g: number;
  b: number;
}

const HEX = /^#([0-9a-f]{3}|[0-9a-f]{6}|[0-9a-f]{8})$/i;

export function isHex(value: unknown): value is string {
  return typeof value === "string" && HEX.test(value.trim());
}

export function parseHex(value: string): Rgb {
  const m = HEX.exec(value.trim());
  if (!m) throw new Error(`Invalid hex colour: ${value}`);
  let h = m[1];
  if (h.length === 3) h = [...h].map((c) => c + c).join("");
  const n = parseInt(h.slice(0, 6), 16);
  return { r: (n >> 16) & 255, g: (n >> 8) & 255, b: n & 255 };
}

export function toHex({ r, g, b }: Rgb): string {
  const c = (v: number) => Math.max(0, Math.min(255, Math.round(v))).toString(16).padStart(2, "0");
  return `#${c(r)}${c(g)}${c(b)}`.toUpperCase();
}

const channel = (v: number) => {
  const s = v / 255;
  return s <= 0.03928 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4;
};

export function luminance(hex: string): number {
  const { r, g, b } = parseHex(hex);
  return 0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b);
}

/** WCAG 2.x contrast ratio, 1..21. */
export function contrastRatio(a: string, b: string): number {
  const l1 = luminance(a);
  const l2 = luminance(b);
  return (Math.max(l1, l2) + 0.05) / (Math.min(l1, l2) + 0.05);
}

/** sRGB-space mix of two colours; `t` is the share of `b`. */
export function mix(a: string, b: string, t: number): string {
  const x = parseHex(a);
  const y = parseHex(b);
  return toHex({ r: x.r + (y.r - x.r) * t, g: x.g + (y.g - x.g) * t, b: x.b + (y.b - x.b) * t });
}

/** Whichever of `light`/`dark` reads better on `background`. */
export function readableOn(background: string, light = "#FFFFFF", dark = "#1A1208"): string {
  return contrastRatio(light, background) >= contrastRatio(dark, background) ? light : dark;
}

/**
 * Move `fg` toward white (when `bg` is dark) or black (when `bg` is light) just far enough to reach
 * `min` contrast against `bg`, keeping the hue. Returns `fg` unchanged when it already passes, and the
 * extreme (white/black) when nothing in between reaches `min`.
 */
export function ensureContrast(fg: string, bg: string, min: number): string {
  if (contrastRatio(fg, bg) >= min) return toHex(parseHex(fg));
  const target = luminance(bg) < 0.5 ? "#FFFFFF" : "#000000";
  if (contrastRatio(target, bg) < min) return target;
  let lo = 0;
  let hi = 1;
  for (let i = 0; i < 24; i += 1) {
    const mid = (lo + hi) / 2;
    if (contrastRatio(mix(fg, target, mid), bg) >= min) hi = mid;
    else lo = mid;
  }
  return mix(fg, target, hi);
}
