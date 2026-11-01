// Maps the app's theme tokens onto the --vr-* custom properties the sandboxed MDX frame paints with.
// The frame is an opaque origin and cannot read the app's CSS variables, so the parent resolves them
// here and sends the resolved values across the postMessage boundary (see postMdxTheme).

/** Frame custom property -> app theme token it mirrors. */
export const MDX_THEME_TOKEN_MAP: Readonly<Record<string, string>> = {
  "--vr-fg": "--paper",
  "--vr-heading": "--paper",
  "--vr-link": "--copper",
  "--vr-muted": "--muted",
  "--vr-label": "--muted-2",
  "--vr-line": "--line",
  "--vr-quote-line": "--line",
  "--vr-code-bg": "--panel-2",
  "--vr-code-fg": "--paper",
  "--vr-callout-bg": "--panel-2",
  "--vr-error": "--danger",
};

// Plain color values only: letters, digits, spaces, and the punctuation colors use (# ( ) , . % -).
// Anything else (quotes, braces, semicolons, url()) is dropped so a token can never inject CSS.
const SAFE_VALUE = /^[#\w\s(),.%/-]{1,80}$/;

/** Resolves the mapped tokens from `root`'s computed style. `read` is injectable for tests. */
export function readMdxThemeVars(
  root: Element,
  read: (el: Element, token: string) => string = (el, token) =>
    getComputedStyle(el).getPropertyValue(token),
): Record<string, string> {
  const vars: Record<string, string> = {};
  for (const [frameVar, token] of Object.entries(MDX_THEME_TOKEN_MAP)) {
    const value = read(root, token).trim();
    if (value && SAFE_VALUE.test(value)) vars[frameVar] = value;
  }
  return vars;
}
