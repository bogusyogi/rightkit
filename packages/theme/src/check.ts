import { SUITE_DEFAULTS, SUITE_MOTION, resolveBrand, type BrandFile, type Mode } from "./brand.js";
import { contrastRatio, isHex } from "./color.js";

export interface ContrastResult {
  id: string;
  mode: Mode;
  fg: string;
  bg: string;
  ratio: number;
  min: number;
  pass: boolean;
}

export interface Thresholds {
  /** Body and accent text (WCAG AA). Default 4.5. */
  text?: number;
  /** Tertiary/ancillary text such as ink-3 and non-text UI. Default 3. */
  ancillary?: number;
}

export interface ContrastReport {
  pass: boolean;
  failures: ContrastResult[];
  results: ContrastResult[];
}

const TEXT_ON = ["plane", "elevated", "chrome"] as const;

/**
 * WCAG contrast gate for a brand file, per mode, against the effective suite surfaces:
 * ink and ink-2 on every surface (text), ink-3 on plane (ancillary), accent text on plane/elevated,
 * accent-ink on the filled accent, status colours on plane. Colours given as rgba() are skipped (alpha is surface-dependent).
 */
export function checkBrandContrast(brand: BrandFile, thresholds: Thresholds = {}): ContrastReport {
  const text = thresholds.text ?? 4.5;
  const ancillary = thresholds.ancillary ?? 3;
  const r = resolveBrand(brand);
  const results: ContrastResult[] = [];
  const push = (id: string, mode: Mode, fg: string | undefined, bg: string | undefined, min: number) => {
    if (!isHex(fg) || !isHex(bg)) return;
    const ratio = contrastRatio(fg, bg);
    results.push({ id, mode, fg, bg, ratio: Math.round(ratio * 100) / 100, min, pass: ratio >= min });
  };
  for (const mode of ["light", "dark"] as const) {
    const t = r.effective[mode];
    const accent = r.palette[mode]["brand-action"];
    for (const surface of TEXT_ON) {
      push(`ink on ${surface}`, mode, t.ink, t[surface], text);
      push(`ink-2 on ${surface}`, mode, t["ink-2"], t[surface], text);
    }
    push("ink-3 on plane", mode, t["ink-3"], t.plane, ancillary);
    for (const surface of ["plane", "elevated"] as const) push(`accent text on ${surface}`, mode, r.palette[mode]["brand-ink"], t[surface], text);
    push("accent-ink on accent", mode, t["accent-ink"], accent, text);
    push("accent on plane (UI)", mode, accent, t.plane, ancillary);
    for (const status of ["ok", "warn", "bad"] as const) push(`${status} on plane`, mode, t[status], t.plane, text);
  }
  const failures = results.filter((x) => !x.pass);
  return { pass: failures.length === 0, failures, results };
}

export interface TokenFinding {
  file: string;
  line: number;
  value: string;
  text: string;
}

export interface ScanOptions {
  /** Regexes; a line matching any is an approved context for colour literals. */
  approvedContexts?: RegExp[];
  /** Literal values always allowed (lower-case). */
  approvedValues?: string[];
}

const COLOR_RE = /#[0-9a-fA-F]{3,8}\b|rgba?\([^)]+\)|hsla?\([^)]+\)/g;

/** Contexts where a colour literal is legitimate: defining a custom property, shadows, gradients, semantic marks. */
export const DEFAULT_APPROVED_CONTEXTS: RegExp[] = [
  /--[a-z0-9-]+\s*:/i,
  /box-shadow:/i,
  /text-shadow:/i,
  /conic-gradient|linear-gradient|radial-gradient/i,
  /color-mix\(in srgb,\s*#[0-9a-fA-F]{3,8}/i,
  /var\(--[a-z0-9-]+,\s*(?:#[0-9a-fA-F]{3,8}|rgba?\([^)]+\))\)/i,
  /rightkit-theme:\s*allow/i,
];

export const DEFAULT_APPROVED_VALUES = ["#fff", "#ffffff", "#000", "#000000", "transparent"];

/**
 * Find hard-coded colours that should be tokens. Donor: MailRight `check-token-colors.ts`, generalised:
 * the scanned text, approved contexts and values are parameters, and a `rightkit-theme: allow` comment waives a line.
 */
export function scanHardcodedColors(file: string, source: string, options: ScanOptions = {}): TokenFinding[] {
  const contexts = options.approvedContexts ?? DEFAULT_APPROVED_CONTEXTS;
  const values = new Set((options.approvedValues ?? DEFAULT_APPROVED_VALUES).map((v) => v.toLowerCase()));
  const findings: TokenFinding[] = [];
  source.split(/\r?\n/).forEach((text, idx) => {
    for (const m of text.matchAll(COLOR_RE)) {
      const value = m[0];
      if (values.has(value.toLowerCase())) continue;
      if (contexts.some((re) => re.test(text))) continue;
      findings.push({ file, line: idx + 1, value, text: text.trim() });
    }
  });
  return findings;
}

/** Required custom properties a generated stylesheet must define, in light and in dark. */
export function requiredVars(brand: BrandFile, format: "suite" | "tailwind" = "suite"): { light: string[]; dark: string[] } {
  if (format === "tailwind") {
    const names = Object.keys(resolveBrand(brand).palette.light).map((k) => `--color-${k}`);
    return { light: names, dark: [] };
  }
  const r = resolveBrand(brand);
  const darkNames = ["--rk-brand-mark", "--rk-brand-ink", "--rk-accent-ink"];
  const lightNames = ["--rk-brand", "--rk-brand-dark", ...darkNames, ...Object.keys(r.palette.light).filter((k) => k !== "brand" && k !== "brand-action").map((k) => `--rk-${k}`)];
  return { light: [...new Set(lightNames)], dark: darkNames };
}

/** Check a stylesheet actually defines what the brand file implies (drift or hand-edit detection). */
export function checkGeneratedCss(css: string, brand: BrandFile): string[] {
  const problems: string[] = [];
  const need = requiredVars(brand);
  for (const name of need.light) if (!new RegExp(`${name.replace(/[-/]/g, "\\$&")}\\s*:`).test(css)) problems.push(`missing ${name}`);
  const darkBlock = css.slice(css.indexOf('[data-theme="dark"]'));
  for (const name of need.dark) if (!new RegExp(`${name.replace(/[-/]/g, "\\$&")}\\s*:`).test(darkBlock)) problems.push(`missing ${name} in dark block`);
  return problems;
}

const normalizeValue = (value: string) => value.replace(/\s+/g, "").toLowerCase();

/**
 * Drift check between the flat suite defaults in this package and `@rightkit/react-shell`'s `shell.css` (pass its text).
 * Every `SUITE_DEFAULTS` entry must be declared as `--rk-<name>` in the light `:root` block and in the dark block
 * (`:root:not([data-theme="light"])` or `:root[data-theme="dark"]`), and every `SUITE_MOTION` entry in the light block,
 * with the same value (case and spacing ignored). Returns one line per problem; empty when they agree.
 */
export function checkSuiteDefaults(shellCss: string): string[] {
  const declared: Record<Mode, Map<string, string>> = { light: new Map(), dark: new Map() };
  for (const rule of shellCss.matchAll(/([^{}]+)\{([^{}]*)\}/g)) {
    // The innermost selector is the last line before `{`; @media wrappers and comments are not selectors.
    const selector = (rule[1] ?? "").replace(/\/\*[\s\S]*?\*\//g, "").trim().split("\n").pop()?.trim() ?? "";
    const mode: Mode | null = selector === ":root"
      ? "light"
      : selector === ':root:not([data-theme="light"])' || selector === ':root[data-theme="dark"]' ? "dark" : null;
    if (!mode) continue;
    for (const decl of (rule[2] ?? "").matchAll(/(--rk-[a-z0-9-]+)\s*:\s*([^;]+);/g)) declared[mode].set(decl[1]!, decl[2]!.trim());
  }
  const problems: string[] = [];
  const compare = (mode: Mode, name: string, expected: string) => {
    const actual = declared[mode].get(name);
    if (actual === undefined) problems.push(`${mode}: ${name} is not declared in shell.css`);
    else if (normalizeValue(actual) !== normalizeValue(expected)) problems.push(`${mode}: ${name} is ${actual} in shell.css, SUITE_DEFAULTS says ${expected}`);
  };
  for (const mode of ["light", "dark"] as const) {
    for (const [key, value] of Object.entries(SUITE_DEFAULTS[mode])) compare(mode, `--rk-${key}`, value);
  }
  for (const [name, value] of Object.entries(SUITE_MOTION)) compare("light", name, value);
  return problems;
}
