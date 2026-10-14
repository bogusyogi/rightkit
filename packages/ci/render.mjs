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

// The step that carries a private Swift package credential (see
// swiftPrivatePackagesConfigureStep in manifest.mjs). A workflow that renders it
// must not be triggered by an event that hands secrets to untrusted code.
const SWIFT_CONFIGURE_STEP_NAME = "- name: Configure private Swift package access";
const UNSAFE_SECRET_TRIGGER_RE = /\b(pull_request_target|workflow_run)\b/;

// The top-level `on:` block: from the `on:` line to the next unindented key.
function onBlock(text) {
  const lines = text.split("\n");
  const start = lines.findIndex((line) => /^on:/.test(line));
  if (start < 0) return "";
  let end = start + 1;
  while (end < lines.length && (lines[end] === "" || /^[\s#]/.test(lines[end]))) end += 1;
  return lines.slice(start, end).join("\n");
}

/**
 * Refuse a rendered workflow that carries the private Swift package credential
 * steps under a trigger that runs with secrets on untrusted input
 * (pull_request_target, workflow_run). GitHub withholds secrets from fork
 * pull_request runs, so those two are the only triggers that could leak it.
 */
function assertSwiftCredentialTriggers(rendered) {
  if (!rendered.includes(SWIFT_CONFIGURE_STEP_NAME)) return;
  const trigger = UNSAFE_SECRET_TRIGGER_RE.exec(onBlock(rendered));
  if (trigger) {
    throw new Error(`render: refusing to render the private Swift package credential steps into a workflow triggered by ${trigger[1]}`);
  }
}

/**
 * Render one template against a context map. Deterministic: same template +
 * same context always produces byte-identical output.
 * Throws if a placeholder has no entry in context (mechanical safety net —
 * never silently emits an unresolved token into a workflow file).
 */
export function renderTemplate(templateText, context) {
  const rendered = templateText.replace(/\r\n?/g, "\n").replace(TOKEN_RE, (full, key) => {
    if (!Object.hasOwn(context, key)) {
      throw new Error(`render: no value provided for placeholder \${${key}}`);
    }
    return String(context[key]);
  });

  const leftover = LEFTOVER_RE.exec(rendered);
  if (leftover) {
    throw new Error(`render: unresolved placeholder ${leftover[0]} remained after substitution`);
  }
  assertSwiftCredentialTriggers(rendered);
  // Optional final job fragments may expand to an empty string. Normalize
  // template EOF after substitution so managed workflows remain byte-stable
  // with exactly one terminating newline.
  return `${rendered.replace(/(?:\r\n|\r|\n)+$/, "")}\n`;
}
