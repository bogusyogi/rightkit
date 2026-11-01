import { mix } from "@rightkit/theme";
import type { ThemeTokens } from "@rightkit/document-core/settings/themes";

/** Lighten (amount > 0) or darken (amount < 0) toward white/black. */
const shade = (hex: string, amount: number): string =>
  amount >= 0 ? mix(hex, "#ffffff", amount) : mix(hex, "#000000", -amount);

// We run a FORKED build of @eigenpal (vendor/docx-editor) re-themed at source so every surface
// reads CSS variables instead of hardcoded slate/white. It consumes TWO var sets we must supply:
// (1) raw `--doc-*` colors for the document surface, toolbar, menus, dialogs (all used — see
// eigenpalTheme.test.ts), and (2) shadcn-style vars consumed as `hsl(var(--x))` by its Radix bits (select
// dropdowns) — these need HSL TRIPLETS, not hex. The editor scopes its vars on `.ep-root`, so we
// re-declare there (plus `:root`); DocxPane injects this at runtime via <style>, winning by source
// order over the bundled stylesheet.
//
// Pure module (no React/editor/CSS imports) so the var-coverage test can import it standalone.

function hexToHslTriplet(hex: string): string {
  const m = /^#?([0-9a-f]{6})$/i.exec(hex.trim());
  if (!m) return "0 0% 0%";
  const n = Number.parseInt(m[1], 16);
  const r = ((n >> 16) & 255) / 255;
  const g = ((n >> 8) & 255) / 255;
  const b = (n & 255) / 255;
  const max = Math.max(r, g, b);
  const min = Math.min(r, g, b);
  const l = (max + min) / 2;
  let h = 0;
  let s = 0;
  if (max !== min) {
    const d = max - min;
    s = l > 0.5 ? d / (2 - max - min) : d / (max + min);
    if (max === r) h = (g - b) / d + (g < b ? 6 : 0);
    else if (max === g) h = (b - r) / d + 2;
    else h = (r - g) / d + 4;
    h /= 6;
  }
  return `${(h * 360).toFixed(1)} ${(s * 100).toFixed(1)}% ${(l * 100).toFixed(1)}%`;
}

export function buildEigenpalThemeCss(
  t: ThemeTokens,
  primary: string,
  mode: "light" | "dark",
): string {
  const H = hexToHslTriplet;
  const dark = mode === "dark";
  const decls = [
    // shadcn chrome vars — HSL triplets, consumed as hsl(var(--x)) by Radix select/menus/dialogs.
    `--background:${H(t.panel)}`,
    `--foreground:${H(t.paper)}`,
    `--card:${H(t.panel)}`,
    `--card-foreground:${H(t.paper)}`,
    `--popover:${H(t.panel2)}`,
    `--popover-foreground:${H(t.paper)}`,
    `--border:${H(t.line)}`,
    `--input:${H(t.line)}`,
    `--muted:${H(t.panel2)}`,
    `--muted-foreground:${H(t.muted)}`,
    `--secondary:${H(t.panel2)}`,
    `--secondary-foreground:${H(t.paper)}`,
    `--accent:${H(t.panel2)}`,
    `--accent-foreground:${H(t.paper)}`,
    `--primary:${H(primary)}`,
    `--primary-foreground:${H(dark ? t.ink : t.paper)}`,
    `--destructive:${H(dark ? "#F0686C" : "#D5484D")}`,
    `--destructive-foreground:${H(t.paper)}`,
    `--ring:${H(primary)}`,
    // raw `--doc-*` palette — the fork reads ALL of these directly (coverage asserted by test).
    `--doc-bg:${t.panel}`,
    `--doc-bg-subtle:${t.panel2}`,
    `--doc-bg-hover:${shade(t.panel2, dark ? 0.05 : -0.05)}`,
    `--doc-bg-input:${t.ink}`,
    `--doc-hover:${shade(t.panel2, dark ? 0.05 : -0.05)}`,
    `--doc-toolbar-bg:${t.panel}`,
    `--doc-border:${t.line}`,
    `--doc-border-input:${t.line}`,
    `--doc-border-dark:${shade(t.line, dark ? 0.1 : -0.12)}`,
    `--doc-border-light:${shade(t.line, dark ? -0.05 : 0.05)}`,
    `--doc-text:${t.paper}`,
    `--doc-text-muted:${t.muted}`,
    `--doc-text-subtle:${t.muted2}`,
    `--doc-text-placeholder:${t.muted2}`,
    `--doc-primary:${primary}`,
    `--doc-primary-hover:${shade(primary, dark ? 0.08 : -0.08)}`,
    `--doc-primary-light:${shade(primary, dark ? -0.32 : 0.34)}`,
    `--doc-link:${primary}`,
    `--doc-error:${dark ? "#F0686C" : "#D5484D"}`,
    `--doc-error-bg:${dark ? "#2A1618" : "#FDECEC"}`,
    `--doc-success:${dark ? "#46C18A" : "#2E9C68"}`,
    `--doc-success-bg:${dark ? "#13261E" : "#EAF7F1"}`,
    `--doc-warning:${dark ? "#E5A24B" : "#C77A20"}`,
    `--doc-warning-bg:${dark ? "#2A2012" : "#FBF1E0"}`,
  ];
  return `:root,.ep-root{${decls.join(";")}}`;
}
