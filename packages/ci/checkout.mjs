// Optional `checkout` manifest block: Windows long-path support and sparse
// checkout for every actions/checkout step in a rendered workflow.
//
//   "checkout": { "longPaths": true, "sparse": ["packages/", "examples/App"] }
//
// longPaths renders, immediately before each checkout step and gated on
// `runner.os == 'Windows'`, a step running `git config --system core.longpaths true`
// (the actions/checkout workaround for "Filename too long", actions/checkout#242).
// sparse renders `sparse-checkout` (cone mode) into each checkout step's `with:`.
// Unset, rendered workflows are byte-identical.

const SPARSE_PATH_RE = /^(?!.*\.\.)(?!\.?\/)[A-Za-z0-9._][A-Za-z0-9._/-]*$/;
const CHECKOUT_KEYS = new Set(["longPaths", "sparse"]);
const CHECKOUT_USES_RE = /^(\s*)(- )?uses: actions\/checkout@/;

export function validateCheckout(checkout, errors) {
  if (checkout === undefined) return;
  if (checkout === null || typeof checkout !== "object" || Array.isArray(checkout)) {
    errors.push("checkout must be an object");
    return;
  }
  for (const key of Object.keys(checkout)) {
    if (!CHECKOUT_KEYS.has(key)) errors.push(`unknown checkout field: ${key}`);
  }
  if (checkout.longPaths !== undefined && typeof checkout.longPaths !== "boolean") {
    errors.push("checkout.longPaths must be a boolean");
  }
  if (checkout.sparse !== undefined) {
    const s = checkout.sparse;
    if (!Array.isArray(s) || s.length === 0 || s.length > 64
      || !s.every((p) => typeof p === "string" && SPARSE_PATH_RE.test(p))
      || new Set(s).size !== s.length) {
      errors.push("checkout.sparse must be a non-empty array (at most 64) of unique safe repository-relative directory paths (letters, digits, '.', '_', '-', '/'; no '..', whitespace, leading '/', or expression syntax)");
    }
  }
}

/** Apply manifest.checkout to every actions/checkout step of a rendered workflow. */
export function applyCheckoutOptions(content, manifest) {
  const opts = manifest.checkout;
  if (!opts || (opts.longPaths !== true && !opts.sparse)) return content;
  const lines = content.split("\n");
  const out = [];
  for (let i = 0; i < lines.length; i += 1) {
    const match = CHECKOUT_USES_RE.exec(lines[i]);
    if (!match) {
      out.push(lines[i]);
      continue;
    }
    const indent = match[1];
    // Step start: the `- uses:` line itself, or the preceding `- name:` line.
    let stepIndent = indent;
    if (!match[2]) {
      stepIndent = indent.slice(0, -2);
      const previous = out[out.length - 1];
      if (previous === undefined || !previous.startsWith(`${stepIndent}- `)) {
        throw new Error("checkout: cannot locate the start of a named actions/checkout step");
      }
    }
    if (opts.longPaths === true) {
      const step = [
        `${stepIndent}- name: Enable Windows long paths`,
        `${stepIndent}  if: runner.os == 'Windows'`,
        `${stepIndent}  shell: bash`,
        `${stepIndent}  run: git config --system core.longpaths true`,
      ];
      const at = match[2] ? out.length : out.length - 1;
      // For a named step, insert before its `- name:` line (and any lines of that header already emitted).
      out.splice(at, 0, ...step);
    }
    out.push(lines[i]);
    // A checkout of another repository (e.g. a tap) keeps its own `with:` and is never sparse.
    let foreign = false;
    for (let j = i + 1; j < lines.length && (lines[j].startsWith(`${stepIndent}  `) || lines[j] === ""); j += 1) {
      if (lines[j].trim().startsWith("repository:")) foreign = true;
    }
    if (opts.sparse && !foreign) {
      const inner = `${stepIndent}  `;
      const sparseLines = [
        `${inner}  sparse-checkout: |`,
        ...opts.sparse.map((p) => `${inner}    ${p}`),
        `${inner}  sparse-checkout-cone-mode: true`,
      ];
      if (lines[i + 1] === `${inner}with:`) {
        out.push(lines[i + 1], ...sparseLines);
        i += 1;
      } else {
        out.push(`${inner}with:`, ...sparseLines);
      }
    }
  }
  return out.join("\n");
}
