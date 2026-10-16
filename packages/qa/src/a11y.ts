import type { QaPage } from './page.js';

/**
 * Accessibility evidence for page QA: WCAG text contrast, the keyboard-focus indicator, and role/name
 * assertions. The verdicts are pure functions over plain data so they are tested without a browser;
 * `measureContrast`, `focusRingEvidence` and `collectAxNodes` do the page-side reads through
 * `QaPage.evaluate`. Mirrors `rightkit_qa::a11y` in the Rust crate (same rules, same thresholds).
 */

export interface Rgba { r: number; g: number; b: number; a: number }

export const WHITE: Rgba = { r: 255, g: 255, b: 255, a: 1 };
export const TRANSPARENT: Rgba = { r: 0, g: 0, b: 0, a: 0 };

const clamp = (v: number, lo: number, hi: number) => Math.min(hi, Math.max(lo, v));

function channel(token: string, scale: number): number | null {
  const t = token.trim();
  const value = t.endsWith('%') ? (Number(t.slice(0, -1)) / 100) * scale : Number(t);
  return t !== '' && Number.isFinite(value) ? clamp(value, 0, scale) : null;
}

function alphaOf(token: string): number | null {
  const t = token.trim();
  const value = t.endsWith('%') ? Number(t.slice(0, -1)) / 100 : Number(t);
  return t !== '' && Number.isFinite(value) ? clamp(value, 0, 1) : null;
}

/**
 * A CSS colour as `getComputedStyle` reports it: `rgb()`/`rgba()` (comma or space syntax, optional
 * `/ alpha`), `color(srgb r g b / a)`, `#rgb`, `#rgba`, `#rrggbb`, `#rrggbbaa`, `transparent`. Anything
 * else (named colours, `oklch`, ...) is `null`.
 */
export function parseCssColor(text: string): Rgba | null {
  const t = text.trim().toLowerCase();
  if (t === 'transparent') return { ...TRANSPARENT };
  if (t.startsWith('#')) {
    const hex = t.slice(1);
    if (!/^[0-9a-f]+$/.test(hex)) return null;
    const nib = (i: number) => parseInt(hex[i], 16) * 17;
    const byte = (i: number) => parseInt(hex.slice(i, i + 2), 16);
    switch (hex.length) {
      case 3: return { r: nib(0), g: nib(1), b: nib(2), a: 1 };
      case 4: return { r: nib(0), g: nib(1), b: nib(2), a: nib(3) / 255 };
      case 6: return { r: byte(0), g: byte(2), b: byte(4), a: 1 };
      case 8: return { r: byte(0), g: byte(2), b: byte(4), a: byte(6) / 255 };
      default: return null;
    }
  }
  const match = /^([a-z]+)\((.*)\)$/.exec(t);
  if (!match) return null;
  const [, func, args] = match;
  const [main, slashAlpha] = args.split('/') as [string, string | undefined];
  const parts = main.split(/[\s,]+/).filter(Boolean);
  if (func === 'rgb' || func === 'rgba') {
    if (parts.length !== 3 && parts.length !== 4) return null;
    if (slashAlpha !== undefined && parts.length === 4) return null;
    const alphaToken = slashAlpha ?? parts[3];
    const a = alphaToken === undefined ? 1 : alphaOf(alphaToken);
    const [r, g, b] = [channel(parts[0], 255), channel(parts[1], 255), channel(parts[2], 255)];
    return r === null || g === null || b === null || a === null ? null : { r, g, b, a };
  }
  if (func === 'color' && parts.length === 4 && parts[0] === 'srgb') {
    const a = slashAlpha === undefined ? 1 : alphaOf(slashAlpha);
    const [r, g, b] = [channel(parts[1], 1), channel(parts[2], 1), channel(parts[3], 1)];
    return r === null || g === null || b === null || a === null ? null : { r: r * 255, g: g * 255, b: b * 255, a };
  }
  return null;
}

/** `top` painted over `under` (source-over). */
export function compositeOver(top: Rgba, under: Rgba): Rgba {
  const a = top.a + under.a * (1 - top.a);
  if (a <= 0) return { ...TRANSPARENT };
  const mix = (f: number, b: number) => (f * top.a + b * under.a * (1 - top.a)) / a;
  return { r: mix(top.r, under.r), g: mix(top.g, under.g), b: mix(top.b, under.b), a };
}

export function toHex(c: Rgba): string {
  return `#${[c.r, c.g, c.b].map((v) => Math.round(v).toString(16).padStart(2, '0')).join('')}`;
}

/** WCAG relative luminance of an opaque colour. */
export function relativeLuminance(c: Rgba): number {
  const lin = (v: number) => { const s = v / 255; return s <= 0.03928 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4; };
  return 0.2126 * lin(c.r) + 0.7152 * lin(c.g) + 0.0722 * lin(c.b);
}

/** WCAG contrast ratio (1 to 21). A translucent `fg` is composited over `bg`; a translucent `bg` over white. */
export function contrastRatio(fg: Rgba, bg: Rgba): number {
  const background = compositeOver(bg, WHITE);
  const foreground = compositeOver(fg, background);
  const [x, y] = [relativeLuminance(foreground), relativeLuminance(background)];
  return (Math.max(x, y) + 0.05) / (Math.min(x, y) + 0.05);
}

/** The colour behind an element: `chain` is its `background-color` then each ancestor's (innermost first), over a white canvas. */
export function effectiveBackground(chain: Rgba[]): Rgba {
  return [...chain].reverse().reduce((under, layer) => compositeOver(layer, under), WHITE);
}

/** WCAG "large text": at least 24 CSS px, or at least 18.66 px (14 pt) and bold (700+). */
export function isLargeText(fontPx: number, fontWeight: number): boolean {
  return fontPx >= 24 || (fontPx >= 18.66 && fontWeight >= 700);
}

export type ContrastLevel = 'AA' | 'AAA';

export function contrastThreshold(level: ContrastLevel, large: boolean): number {
  const key = String(level).trim().toUpperCase();
  if (key === 'AA') return large ? 3 : 4.5;
  if (key === 'AAA') return large ? 4.5 : 7;
  throw new TypeError(`contrast level must be AA or AAA, got '${level}'`);
}

/** What the page reports for one element (see `measureContrast`). */
export interface ContrastProbe {
  found: boolean;
  color?: string;
  chain?: string[];
  fontSize?: number;
  fontWeight?: number;
  opacity?: number;
  hasImage?: boolean;
}

export interface ContrastOptions {
  /** Default `AA`. */
  level?: ContrastLevel;
  /** An explicit minimum ratio; wins over `level`. */
  min?: number;
  /** Treat the text as large (default: decided from the element's font size and weight). */
  large?: boolean;
  /** Return the verdict without throwing when it is below the minimum. */
  reportOnly?: boolean;
}

export interface ContrastReport {
  ratio: number;
  min: number;
  passes: boolean;
  large: boolean;
  fg: string;
  bg: string;
  fontPx: number;
  fontWeight: number;
  backgroundImage: boolean;
}

export class ContrastFailure extends Error {
  constructor(readonly selector: string, readonly report: ContrastReport) {
    super(
      `contrast of '${selector}' is ${report.ratio.toFixed(2)}:1, below the ${report.min}:1 minimum (text ${report.fg}, background ${report.bg}`
      + `${report.backgroundImage ? '; a background image is also present, so the measured background is approximate' : ''})`,
    );
    this.name = 'ContrastFailure';
  }
}

/** Turn a probe into a verdict. Throws when the element is missing, a colour cannot be read, or the ratio is below the minimum (unless `reportOnly`). */
export function contrastReport(selector: string, probe: ContrastProbe, options: ContrastOptions = {}): ContrastReport {
  if (!probe.found) throw new Error(`contrast: no element matches '${selector}'`);
  const text = parseCssColor(probe.color ?? '');
  if (!text) throw new Error(`contrast: cannot read text colour '${probe.color}' of '${selector}'`);
  const layers = (probe.chain ?? []).map((value) => {
    const parsed = parseCssColor(value);
    if (!parsed) throw new Error(`contrast: cannot read background colour '${value}' behind '${selector}'`);
    return parsed;
  });
  const bg = effectiveBackground(layers);
  const opacity = clamp(probe.opacity ?? 1, 0, 1);
  const fg = compositeOver({ ...text, a: text.a * opacity }, bg);
  const ratio = contrastRatio(fg, bg);
  const fontPx = probe.fontSize ?? 16;
  const fontWeight = probe.fontWeight ?? 400;
  const large = options.large ?? isLargeText(fontPx, fontWeight);
  const min = options.min ?? contrastThreshold(options.level ?? 'AA', large);
  const report: ContrastReport = {
    ratio: Math.round(ratio * 100) / 100, min, passes: ratio >= min, large,
    fg: toHex(fg), bg: toHex(bg), fontPx, fontWeight, backgroundImage: probe.hasImage === true,
  };
  if (!report.passes && !options.reportOnly) throw new ContrastFailure(selector, report);
  return report;
}

/** Measure the contrast of the first element matching `selector` on `page`. */
export async function measureContrast(page: QaPage, selector: string, options: ContrastOptions = {}): Promise<ContrastReport> {
  const probe = await page.evaluate((sel: string): ContrastProbe => {
    const el = document.querySelector(sel);
    if (!el) return { found: false };
    const cs = getComputedStyle(el);
    const chain: string[] = [];
    let hasImage = false;
    for (let n: Element | null = el; n; n = n.parentElement) {
      const s = getComputedStyle(n);
      chain.push(s.backgroundColor);
      if (s.backgroundImage && s.backgroundImage !== 'none') hasImage = true;
    }
    return { found: true, color: cs.color, fontSize: parseFloat(cs.fontSize), fontWeight: parseFloat(cs.fontWeight) || 400, opacity: parseFloat(cs.opacity), chain, hasImage };
  }, selector);
  return contrastReport(selector, probe, options);
}

// -------------------------------------------------------------------- focus

/** The focused element's computed indicator, as read from the page. */
export interface FocusProbe {
  focused: boolean;
  matches?: boolean | null;
  tag?: string;
  id?: string | null;
  role?: string | null;
  name?: string;
  focusVisible?: boolean | null;
  outlineStyle?: string;
  outlineWidth?: string;
  outlineColor?: string;
  outlineOffset?: string;
  boxShadow?: string;
}

export interface FocusRingEvidence {
  focused: boolean;
  /** True when the focused element paints an outline or a box-shadow. */
  visible: boolean;
  kind: 'outline' | 'box-shadow' | 'none';
  reason: string;
  matches: boolean | null;
  focusVisible: boolean | null;
  tag: string | null;
  id: string | null;
  role: string | null;
  name: string | null;
  outline: { style: string; width: string; color: string; offset: string };
  boxShadow: string;
}

const pxValue = (token: string) => { const m = /^\s*(-?[\d.]+)px\s*$/.exec(token); return m ? Number(m[1]) : null; };

function splitTopLevel(text: string, separator: RegExp): string[] {
  const out: string[] = [];
  let current = '';
  let depth = 0;
  for (const ch of text) {
    if (ch === '(') depth++;
    if (ch === ')') depth--;
    if (depth === 0 && separator.test(ch)) { if (current) out.push(current); current = ''; } else current += ch;
  }
  if (current.trim()) out.push(current);
  return out;
}

/** True when a computed `box-shadow` paints something: a non-transparent colour and a non-zero offset, blur or spread. */
export function boxShadowVisible(boxShadow: string): boolean {
  const text = boxShadow.trim();
  if (text === '' || text === 'none') return false;
  return splitTopLevel(text, /,/).some((one) => {
    const tokens = splitTopLevel(one, /\s/);
    const colour = tokens.map(parseCssColor).find((c) => c !== null) ?? null;
    const lengths = tokens.map(pxValue).filter((v): v is number => v !== null);
    return (colour === null || colour.a > 0) && lengths.some((v) => v !== 0);
  });
}

/** Does the focused element show a visible indicator, and which kind. No indicator is `visible: false`, not an error. */
export function focusRingEvidence(probe: FocusProbe): FocusRingEvidence {
  const style = probe.outlineStyle ?? '';
  const outlineWidth = probe.outlineWidth ?? '';
  const outlineColor = probe.outlineColor ?? '';
  const boxShadow = probe.boxShadow ?? '';
  const empty = { matches: null, focusVisible: null, tag: null, id: null, role: null, name: null, outline: { style, width: outlineWidth, color: outlineColor, offset: probe.outlineOffset ?? '' }, boxShadow };
  if (!probe.focused) return { ...empty, focused: false, visible: false, kind: 'none', reason: 'no element has keyboard focus' };
  const colour = parseCssColor(outlineColor);
  const outline = !['', 'none', 'hidden'].includes(style) && (pxValue(outlineWidth) ?? 0) > 0 && (colour === null || colour.a > 0);
  const shadow = boxShadowVisible(boxShadow);
  const kind = outline ? 'outline' : shadow ? 'box-shadow' : 'none';
  const reason = outline ? `outline ${style} ${outlineWidth} ${outlineColor}`
    : shadow ? `box-shadow ${boxShadow}`
      : `outline is '${style} ${outlineWidth}' and box-shadow is '${boxShadow}'`;
  return {
    ...empty, focused: true, visible: kind !== 'none', kind, reason,
    matches: probe.matches ?? null, focusVisible: probe.focusVisible ?? null,
    tag: probe.tag ?? null, id: probe.id ?? null, role: probe.role ?? null, name: probe.name ?? null,
  };
}

/** Read the focused element's indicator on `page`. With `selector`, `matches` says whether it is that element. */
export async function readFocusRing(page: QaPage, selector?: string): Promise<FocusRingEvidence> {
  const probe = await page.evaluate((sel: string | null): FocusProbe => {
    const el = document.activeElement;
    if (!el || el === document.body || el === document.documentElement) return { focused: false, matches: null };
    const cs = getComputedStyle(el);
    let focusVisible: boolean | null = null;
    try { focusVisible = el.matches(':focus-visible'); } catch { /* unsupported */ }
    return {
      focused: true, matches: sel ? el.matches(sel) : null, tag: el.tagName.toLowerCase(), id: el.id || null,
      role: el.getAttribute('role'),
      name: (el.getAttribute('aria-label') || (el as HTMLElement).innerText || (el as HTMLInputElement).value || '').trim().slice(0, 80),
      focusVisible, outlineStyle: cs.outlineStyle, outlineWidth: cs.outlineWidth, outlineColor: cs.outlineColor,
      outlineOffset: cs.outlineOffset, boxShadow: cs.boxShadow,
    };
  }, selector ?? null);
  return focusRingEvidence(probe);
}

// ----------------------------------------------------------------------- ax

export interface AxNode { role: string; name: string; disabled?: boolean; [key: string]: unknown }

export interface AxQuery {
  /** Role, compared case-insensitively. */
  role?: string;
  /** Accessible name, equal after trimming and collapsing whitespace. */
  name?: string;
  /** Accessible name contains this text (case-insensitive). */
  nameContains?: string;
  /** The accessible name must not be empty. */
  named?: boolean;
  disabled?: boolean;
  /** At least this many nodes (default 1). */
  minCount?: number;
  maxCount?: number;
}

const collapse = (s: string) => s.trim().split(/\s+/).filter(Boolean).join(' ');

export function matchAxNodes(nodes: readonly AxNode[], query: AxQuery): AxNode[] {
  return nodes.filter((node) => {
    const name = collapse(node.name ?? '');
    return (query.role === undefined || String(node.role ?? '').toLowerCase() === query.role.trim().toLowerCase())
      && (query.name === undefined || name === collapse(query.name))
      && (query.nameContains === undefined || name.toLowerCase().includes(collapse(query.nameContains).toLowerCase()))
      && (!query.named || name !== '')
      && (query.disabled === undefined || (node.disabled === true) === query.disabled);
  });
}

/** Assert a role/name query over `nodes`; the error lists the roles and names that exist. Returns the matches. */
export function assertAx(nodes: readonly AxNode[], query: AxQuery): AxNode[] {
  if (query.role === undefined && query.name === undefined && query.nameContains === undefined && !query.named) {
    throw new TypeError('assertAx needs at least one of role, name, nameContains or named');
  }
  const found = matchAxNodes(nodes, query);
  const min = query.minCount ?? 1;
  if (found.length < min || (query.maxCount !== undefined && found.length > query.maxCount)) {
    const seen = nodes.slice(0, 40).map((n) => `${n.role ?? '?'}:'${collapse(n.name ?? '')}'`).join(', ');
    throw new Error(`ax: expected ${min}${query.maxCount !== undefined ? `..${query.maxCount}` : ''} node(s) matching ${JSON.stringify(query)}, found ${found.length} among ${nodes.length} node(s): [${seen}]`);
  }
  return found;
}

/** The page's interactive and landmark elements with their role and accessible name (implicit roles for common tags; the control server's `ax` shape). */
export function collectAxNodes(page: QaPage): Promise<AxNode[]> {
  return page.evaluate((): AxNode[] => {
    const implicit: Record<string, string> = { A: 'link', BUTTON: 'button', INPUT: 'textbox', SELECT: 'combobox', TEXTAREA: 'textbox', H1: 'heading', H2: 'heading', H3: 'heading', H4: 'heading', LABEL: 'label', SUMMARY: 'button' };
    const all = document.querySelectorAll('a,button,input,select,textarea,summary,[role],[tabindex],h1,h2,h3,h4,label');
    return Array.from(all).slice(0, 500).map((e) => {
      const role = e.getAttribute('role') || implicit[e.tagName] || e.tagName.toLowerCase();
      const name = (e.getAttribute('aria-label') || (e as HTMLElement).innerText || (e as HTMLInputElement).value || (e as HTMLInputElement).placeholder || '').trim().slice(0, 120);
      return { role, name, disabled: !!(e as HTMLInputElement).disabled };
    });
  });
}
