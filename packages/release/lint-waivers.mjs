// Rust lint waivers (bogusyogi/rightkit#1830, first slice): a port of Legion's `legion-dev check-waivers` (engine/bins/legion-dev/src/checks/waivers.rs at
// db1f28bf) for every repository the gate checks. A waiver is any of:
//   - a `#[allow(...)]`, `#![allow(...)]`, `#[expect(...)]` or `#![expect(...)]` attribute in Rust code, or one of them inside `cfg_attr(...)`;
//   - a `[lints]` / `[workspace.lints]` level set to "allow" in a Cargo.toml;
//   - a rustflags entry that disables lints (-A, --allow, --cap-lints) in a Cargo.toml or .cargo/config(.toml).
// Rust attributes are matched on code only: comments, string literals (raw and byte strings too) and char literals are blanked first, keeping every
// newline so line numbers stay exact. TOML comments are blanked; TOML strings are kept (a level or a flag is a string).
// Not waivers here: `#[warn(...)]` and a "warn" level. The gate's clippy rows run with `-D warnings` (deterministic-scans.mjs clippyArgs), which turns
// every warning into an error, so a lint lowered to warn still fails; only allow, expect and a lint cap silence it.
// Fixture trees (`/tests/fixtures/`) are skipped, as in Legion's check: they hold waivers on purpose, as test input.

const WAIVER = /#!?\[\s*(?:allow|expect)\s*\(/g;
const CFG_ATTR = /#!?\[\s*cfg_attr\s*\(/g;
const NESTED_WAIVER = /\b(?:allow|expect)\s*\(/;
const TABLE = /^\s*\[\[?\s*([^[\]]+?)\s*\]\]?\s*$/;
const KEY_VALUE = /^\s*([A-Za-z0-9_.-]+)\s*=\s*(.*?)\s*$/;
const INLINE_ALLOW = /=\s*["']allow["']/;
const RUSTFLAGS_KEY = /^\s*rustflags\s*=/;
const QUOTED = /"([^"\n]*)"|'([^'\n]*)'/g;

export const WAIVER_REASONS = Object.freeze({
  attribute: "lint waiver attribute (allow/expect)",
  cfgAttr: "cfg_attr applies a lint waiver (allow/expect)",
  level: "lint level set to allow",
  flag: "rustflags disable lints (-A, --allow or --cap-lints)",
});

/** The kind of file the scan reads, or null for any other file. */
export function waiverKindOf(file) {
  if (/(^|\/)tests\/fixtures\//.test(file)) return null;
  if (file.endsWith(".rs")) return "rust";
  if (/(^|\/)Cargo\.toml$/.test(file)) return "manifest";
  if (/(^|\/)\.cargo\/config(\.toml)?$/.test(file)) return "config";
  return null;
}

const isIdent = (c) => c !== undefined && /[\p{L}\p{N}_]/u.test(c);
const blank = (out, from, to) => { for (let k = from; k < to && k < out.length; k++) if (out[k] !== "\n") out[k] = " "; };

/** Blanks comments (nested block comments too), string contents and char literal contents in Rust source, keeping newlines. */
export function maskRust(text) {
  const c = Array.from(text); const n = c.length; const out = c.slice();
  let i = 0;
  while (i < n) {
    const ch = c[i];
    if (ch === "/" && c[i + 1] === "/") { let e = i; while (e < n && c[e] !== "\n") e++; blank(out, i, e); i = e; continue; }
    if (ch === "/" && c[i + 1] === "*") {
      let depth = 0; let k = i;
      while (k < n) {
        if (c[k] === "/" && c[k + 1] === "*") { depth++; k += 2; }
        else if (c[k] === "*" && c[k + 1] === "/") { depth--; k += 2; if (depth === 0) break; }
        else k++;
      }
      blank(out, i, Math.min(k, n)); i = Math.min(k, n); continue;
    }
    // raw strings: r"..", r#".."#, br"..", cr".."
    {
      const r = (c[i] === "b" || c[i] === "c") ? i + 1 : i;
      if (c[r] === "r" && !(i > 0 && isIdent(c[i - 1]))) {
        let k = r + 1; while (c[k] === "#") k++;
        if (c[k] === '"') {
          const hashes = k - r - 1; let e = k + 1;
          while (e < n && !(c[e] === '"' && Array.from({ length: hashes }, (_, h) => c[e + 1 + h] === "#").every(Boolean))) e++;
          blank(out, k + 1, e); i = Math.min(e + 1 + hashes, n); continue;
        }
      }
    }
    if (ch === '"') { let e = i + 1; while (e < n) { if (c[e] === "\\") e += 2; else if (c[e] === '"') break; else e++; } blank(out, i + 1, e); i = Math.min(e + 1, n); continue; }
    if (ch === "'") {
      let close = null;
      if (c[i + 1] === "\\") { let k = i + 3; while (k < n && c[k] !== "'" && c[k] !== "\n") k++; if (k < n && c[k] === "'") close = k; }
      else if (c[i + 1] !== undefined && c[i + 1] !== "\n" && c[i + 2] === "'") close = i + 2;
      if (close !== null) { blank(out, i + 1, close); i = close + 1; continue; }
      i++; continue; // a lifetime
    }
    i++;
  }
  return out.join("");
}

/** Blanks TOML comments, keeping strings (basic, literal, multi-line) and newlines. */
export function maskTomlComments(text) {
  const c = Array.from(text); const n = c.length; const out = c.slice();
  let i = 0;
  while (i < n) {
    if (c[i] === "#") { let e = i; while (e < n && c[e] !== "\n") e++; blank(out, i, e); i = e; continue; }
    if (c[i] === '"' || c[i] === "'") {
      const q = c[i];
      if (c[i + 1] === q && c[i + 2] === q) { let k = i + 3; while (k < n && !(c[k] === q && c[k + 1] === q && c[k + 2] === q)) k += (q === '"' && c[k] === "\\") ? 2 : 1; i = Math.min(k + 3, n); continue; }
      let k = i + 1; while (k < n && c[k] !== "\n") { if (q === '"' && c[k] === "\\") k += 2; else if (c[k] === q) { k++; break; } else k++; }
      i = Math.min(k, n); continue;
    }
    i++;
  }
  return out.join("");
}

const lineOf = (text, offset) => text.slice(0, offset).split("\n").length;
const isAllowFlag = (t) => t.startsWith("-A") || t === "--allow" || t.startsWith("--allow=") || t === "--cap-lints" || t.startsWith("--cap-lints=");
const flagsOnLine = (line) => [...line.matchAll(QUOTED)].some((m) => String(m[1] ?? m[2] ?? "").split(/\s+/).filter(Boolean).some(isAllowFlag));

function rustWaivers(text) {
  const masked = maskRust(text); const out = [];
  for (const m of masked.matchAll(WAIVER)) out.push({ line: lineOf(masked, m.index), reason: WAIVER_REASONS.attribute });
  for (const m of masked.matchAll(CFG_ATTR)) {
    let depth = 1; let k = m.index + m[0].length;
    while (k < masked.length && depth > 0) { if (masked[k] === "(") depth++; else if (masked[k] === ")") depth--; k++; }
    if (NESTED_WAIVER.test(masked.slice(m.index + m[0].length, depth === 0 ? k - 1 : masked.length))) out.push({ line: lineOf(masked, m.index), reason: WAIVER_REASONS.cfgAttr });
  }
  return out;
}

function manifestWaivers(text, kind) {
  const masked = maskTomlComments(text); const out = []; const lines = masked.split("\n");
  if (kind === "manifest") {
    let table = "";
    lines.forEach((line, i) => {
      const t = TABLE.exec(line); if (t) { table = t[1]; return; }
      const kv = KEY_VALUE.exec(line); if (!kv) return;
      const keyPath = table ? `${table}.${kv[1]}` : kv[1];
      const lintsPath = keyPath === "lints" || keyPath.startsWith("lints.") || keyPath === "workspace.lints" || keyPath.startsWith("workspace.lints.");
      const v = kv[2].trim();
      const allows = v.startsWith("{") ? INLINE_ALLOW.test(v) : v.replace(/^["']|["']$/g, "") === "allow";
      if (lintsPath && allows) out.push({ line: i + 1, reason: `${WAIVER_REASONS.level} at ${keyPath}` });
    });
  }
  let inArray = false;
  lines.forEach((line, i) => {
    if (kind === "manifest" && !inArray) { const m = RUSTFLAGS_KEY.exec(line); if (!m) return; inArray = line.slice(m[0].length).trimStart().startsWith("["); }
    if (flagsOnLine(line)) out.push({ line: i + 1, reason: WAIVER_REASONS.flag });
    if (kind === "manifest" && inArray && line.replace(QUOTED, "").includes("]")) inArray = false;
  });
  return out;
}

/** The waivers in one file's text: [{ line, reason }] in line order. `kind` from waiverKindOf. */
export function waiversIn(text, kind) {
  const found = kind === "rust" ? rustWaivers(text) : kind ? manifestWaivers(text, kind) : [];
  return found.sort((a, b) => a.line - b.line);
}
