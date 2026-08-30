// Shared render engine: template text + a flat context map -> rendered YAML.
// Templates use `${TOKEN}` (single brace, upper-snake-case) placeholders.
// GitHub Actions expressions use `${{ ... }}` (double brace) and are never
// touched by this substitution — the two syntaxes cannot collide because our
// token regex requires an upper-case/underscore character immediately after
// `${`, and `${{` is followed by another `{`.

const TOKEN_RE = /\$\{([A-Z][A-Z0-9_]*)\}/g;
const LEFTOVER_RE = /\$\{[A-Z][A-Z0-9_]*\}/;

// First line of every rendered template (see templates/*.yml). Used to
// identify right-git-managed files for drift/uninstall without a separate
// state file — the marker lives in the file itself.
export const MANAGED_MARKER = "# Managed by right-git";

/**
 * Render one template against a context map. Deterministic: same template +
 * same context always produces byte-identical output.
 * Throws if a placeholder has no entry in context (mechanical safety net —
 * never silently emits an unresolved token into a workflow file).
 */
export function renderTemplate(templateText, context) {
  const rendered = templateText.replace(TOKEN_RE, (full, key) => {
    if (!Object.hasOwn(context, key)) {
      throw new Error(`render: no value provided for placeholder \${${key}}`);
    }
    return String(context[key]);
  });

  const leftover = LEFTOVER_RE.exec(rendered);
  if (leftover) {
    throw new Error(`render: unresolved placeholder ${leftover[0]} remained after substitution`);
  }
  // Optional final job fragments may expand to an empty string. Normalize
  // template EOF after substitution so managed workflows remain byte-stable
  // with exactly one terminating newline.
  return `${rendered.replace(/(?:\r\n|\r|\n)+$/, "")}\n`;
}
