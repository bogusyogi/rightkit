// Source-structure helpers for the release-switch scan (rightkit#937, #939, #1045, #1169). Pure text functions, no I/O except
// `buildScriptRefs`, which reads the build scripts it is given. They answer three questions the line-oriented source walk cannot:
//
//   - Is this position inside an item the Rust compiler drops from a release build? (`rustCfgGatedRanges`, `rustFileGates`)
//     Only an attribute `#[cfg(...)]` / `#![cfg(...)]` counts. `cfg!(...)` is a runtime boolean over a live branch: the code
//     inside it is compiled into the release binary, so it is never an enclosure.
//   - Is this position inside a Swift `#if <CONDITION>` branch that requires the declared condition? (`swiftConditionEnclosed`)
//   - Which paths does a build script compile or watch? (`buildScriptRefs`: include!/include_str!/include_bytes!, cc::Build
//     file/files/include, `cargo:rerun-if-changed`, plus the manifest's `build = "..."`.)
import { existsSync, readFileSync } from "node:fs";
import path from "node:path";

// ---------------------------------------------------------------------------------------------------------------------
// Rust.

// Same length as `text`: comments, string literals and char literals become spaces (newlines kept), so brace and paren counting
// never trips on `"}"`, `'{'` or `// {`, and every index still points at the same character of the original text.
export function maskRust(text) {
  return String(text).replace(/\/\/[^\n]*|\/\*[\s\S]*?\*\/|b?r(#*)"[\s\S]*?"\1|b?"(?:\\[\s\S]|[^"\\])*"|b?'(?:\\[^']*|[^\\'\n])'/g, (m) => m.replace(/[^\n]/g, " "));
}

const OPEN = { "(": ")", "[": "]", "{": "}" };
const CLOSE = new Set([")", "]", "}"]);

// Index of the bracket that closes the one at `open` (masked text), or -1.
function matchingClose(masked, open) {
  let depth = 0;
  for (let i = open; i < masked.length; i += 1) {
    const c = masked[i];
    if (OPEN[c]) depth += 1;
    else if (CLOSE.has(c)) { depth -= 1; if (depth === 0) return i; }
  }
  return -1;
}

function splitTopLevel(text, separator) {
  const parts = []; let depth = 0; let start = 0;
  for (let i = 0; i < text.length; i += 1) {
    const c = text[i];
    if (OPEN[c]) depth += 1;
    else if (CLOSE.has(c)) depth -= 1;
    else if (depth === 0 && text.startsWith(separator, i)) { parts.push(text.slice(start, i)); start = i + separator.length; i += separator.length - 1; }
  }
  parts.push(text.slice(start));
  return parts;
}

const squash = (s) => String(s).replace(/\s+/g, "");

// Does the cfg predicate `expr` hold only when `predicate` holds? `predicate` is one of `feature = "x"`, `test`, `debug_assertions`.
// `all(..)` needs one argument that implies it, `any(..)` needs every argument to, `not(..)` never does.
export function cfgImplies(expr, predicate) {
  const e = String(expr).trim();
  const call = /^(all|any|not)\s*\(([\s\S]*)\)$/.exec(e);
  if (call) {
    if (call[1] === "not") return false;
    const args = splitTopLevel(call[2], ",").map((a) => a.trim()).filter(Boolean);
    if (args.length === 0) return false;
    return call[1] === "all" ? args.some((a) => cfgImplies(a, predicate)) : args.every((a) => cfgImplies(a, predicate));
  }
  return squash(e) === squash(predicate);
}

/** The cfg predicate a switches.json `compiledOut` value (`feature:<n>`, `cfg:test`, `cfg:debug_assertions`) stands for, or null. */
export function cfgPredicateOf(compiledOut) {
  const feature = /^feature:([A-Za-z0-9_-]+)$/.exec(compiledOut ?? "");
  if (feature) return `feature = "${feature[1]}"`;
  if (compiledOut === "cfg:test") return "test";
  if (compiledOut === "cfg:debug_assertions") return "debug_assertions";
  return null;
}

// Start of the unmatched `{` that encloses `index` (masked text), and its closing brace; null at file level.
function enclosingBlock(masked, index) {
  let depth = 0;
  for (let i = index - 1; i >= 0; i -= 1) {
    if (masked[i] === "}") depth += 1;
    else if (masked[i] === "{") {
      if (depth === 0) { const close = matchingClose(masked, i); return close < 0 ? [i, masked.length] : [i, close + 1]; }
      depth -= 1;
    }
  }
  return null;
}

// The end (exclusive) of the item or statement that starts at `from`: the first `;` outside every bracket, or the `}` that closes
// the first top-level `{ }` group (`fn`, `impl`, `mod {}`, a block).
function itemEnd(masked, from) {
  let depth = 0; let sawBlock = false;
  for (let i = from; i < masked.length; i += 1) {
    const c = masked[i];
    if (OPEN[c]) { if (c === "{" && depth === 0) sawBlock = true; depth += 1; }
    else if (CLOSE.has(c)) { depth -= 1; if (depth < 0) return i; if (depth === 0 && sawBlock && c === "}") return i + 1; }
    else if (c === ";" && depth === 0) return i + 1;
  }
  return masked.length;
}

// Every `#[cfg(...)]` / `#![cfg(...)]` in a Rust file: { start, end, inner (the cfg predicate text), inside (true for `#!`) }.
function cfgAttributes(text, masked) {
  const out = [];
  for (const m of masked.matchAll(/#(!?)\s*\[\s*cfg\s*\(/g)) {
    const open = m.index + m[0].length - 1;
    const close = matchingClose(masked, open);
    if (close < 0) continue;
    const bracket = /^\s*\]/.exec(masked.slice(close + 1));
    if (!bracket) continue;
    out.push({ start: m.index, end: close + 1 + bracket[0].length, inner: text.slice(open + 1, close), inside: m[1] === "!" });
  }
  return out;
}

/**
 * The [start, end) ranges of `text` that exist only when `predicate` holds: the item or statement after each outer
 * `#[cfg(P)]` (further attributes skipped), the block around an inner `#![cfg(P)]` (the whole file when it opens the file).
 */
export function rustCfgGatedRanges(text, predicate) {
  const masked = maskRust(text);
  const ranges = [];
  for (const attr of cfgAttributes(text, masked)) {
    if (!cfgImplies(attr.inner, predicate)) continue;
    if (attr.inside) {
      const block = enclosingBlock(masked, attr.start);
      ranges.push(block ?? [0, text.length]);
      continue;
    }
    let from = attr.end;
    for (let again = true; again;) {
      again = false;
      const next = /^\s*#\s*\[/.exec(masked.slice(from));
      if (next) { const close = matchingClose(masked, from + next[0].length - 1); if (close >= 0) { from = close + 1; again = true; } }
    }
    ranges.push([attr.end, itemEnd(masked, from)]);
  }
  return ranges;
}

// `mod name;` declarations of a file with the cfg predicate texts written on them (innermost first is not needed: all are ANDed).
function moduleDeclarations(text) {
  const masked = maskRust(text);
  const out = [];
  for (const m of masked.matchAll(/((?:#\s*\[[^\]]*\]\s*)*)(?:pub(?:\s*\([^)]*\))?\s+)?\bmod\s+([A-Za-z_][A-Za-z0-9_]*)\s*;/g)) {
    const exprs = [];
    for (const attr of m[1].matchAll(/#\s*\[\s*cfg\s*\(/g)) {
      const open = m.index + attr.index + attr[0].length - 1;
      const close = matchingClose(masked, open);
      if (close >= 0) exprs.push(text.slice(open + 1, close));
    }
    out.push({ name: m[2], exprs });
  }
  return out;
}

// The files a `mod name;` in `file` names: `<base>/name.rs` or `<base>/name/mod.rs`, with `<base>` the directory of
// lib.rs/main.rs/mod.rs, else the directory named after the file.
function moduleFiles(file, name, exists) {
  const dir = path.dirname(file); const stem = path.basename(file, ".rs");
  const base = ["lib", "main", "mod"].includes(stem) ? dir : path.join(dir, stem);
  return [path.join(base, `${name}.rs`), path.join(base, name, "mod.rs")].filter((candidate) => exists(candidate));
}

/**
 * Per Rust file, the cfg predicate texts that gate the whole file because a parent declares it `#[cfg(..)] mod name;`
 * (followed through nested `mod` declarations). `files` is { file: absolute path, text }[]. Returns Map<file, string[]>.
 */
export function rustFileGates(files) {
  const texts = new Map(files.map(({ file, text }) => [file, text]));
  const exists = (candidate) => texts.has(candidate);
  const declarations = new Map([...texts].map(([file, text]) => [file, moduleDeclarations(text)]));
  const gates = new Map();
  for (let changed = true; changed;) {
    changed = false;
    for (const [file, decls] of declarations) {
      const inherited = gates.get(file) ?? [];
      for (const decl of decls) {
        const wanted = [...decl.exprs, ...inherited];
        if (wanted.length === 0) continue;
        for (const child of moduleFiles(file, decl.name, exists)) {
          const have = gates.get(child) ?? [];
          const missing = wanted.filter((expr) => !have.includes(expr));
          if (missing.length) { gates.set(child, [...have, ...missing]); changed = true; }
        }
      }
    }
  }
  return gates;
}

// ---------------------------------------------------------------------------------------------------------------------
// Swift.

function outerParenthesised(e) {
  if (!e.startsWith("(") || !e.endsWith(")")) return false;
  let depth = 0;
  for (let i = 0; i < e.length; i += 1) {
    if (e[i] === "(") depth += 1;
    else if (e[i] === ")") { depth -= 1; if (depth === 0 && i < e.length - 1) return false; }
  }
  return true;
}

// Does the Swift compilation-condition expression hold only when `condition` is defined? `A && COND` and `COND` do; `COND || A`,
// `!COND` and `os(macOS)` do not.
export function swiftExprRequires(expr, condition) {
  const e = String(expr).trim();
  const ors = splitTopLevel(e, "||");
  if (ors.length > 1) return ors.every((part) => swiftExprRequires(part, condition));
  const ands = splitTopLevel(e, "&&");
  if (ands.length > 1) return ands.some((part) => swiftExprRequires(part, condition));
  if (outerParenthesised(e)) return swiftExprRequires(e.slice(1, -1), condition);
  return e === condition;
}

/** Does `text`'s position `index` sit in a Swift `#if` / `#elseif` branch that requires `condition`? (`#else` never does.) */
export function swiftConditionEnclosed(text, index, condition) {
  const stack = [];
  const directive = /^[ \t]*#(if|elseif|else|endif)\b([^\n]*)/gm;
  for (let m = directive.exec(text); m && m.index < index; m = directive.exec(text)) {
    const expr = m[2].replace(/\/\/.*$/, "").trim();
    if (m[1] === "if") stack.push(swiftExprRequires(expr, condition));
    else if (m[1] === "elseif") { if (stack.length) stack[stack.length - 1] = swiftExprRequires(expr, condition); }
    else if (m[1] === "else") { if (stack.length) stack[stack.length - 1] = false; }
    else stack.pop();
  }
  return stack.some(Boolean);
}

// ---------------------------------------------------------------------------------------------------------------------
// Build scripts.

const quoted = (text) => [...String(text).matchAll(/"((?:\\.|[^"\\])*)"/g)].map((m) => m[1]);

/**
 * Existing paths a crate's build script compiles or watches: string literals given to `include!`/`include_str!`/`include_bytes!`
 * (relative to the build script's directory), `cc::Build` `file`/`files`/`include`/`flag("-I..")`, and `cargo:rerun-if-changed=` /
 * `cargo::rerun-if-changed=` (relative to the crate directory, which is the script's working directory), plus the script file
 * itself. `dirs` are crate directories; each one's `build.rs` and a `build = "<path>"` of its Cargo.toml are read.
 * Not followed: a path assembled at run time (`format!`, `join` of a variable) and anything a script imports by `mod`.
 */
export function buildScriptRefs(dirs) {
  const refs = new Set();
  for (const dir of new Set(dirs)) {
    const scripts = new Set();
    if (existsSync(path.join(dir, "build.rs"))) scripts.add(path.join(dir, "build.rs"));
    try {
      const declared = /^\s*build\s*=\s*"([^"]+)"/m.exec(readFileSync(path.join(dir, "Cargo.toml"), "utf8"));
      if (declared && existsSync(path.resolve(dir, declared[1]))) scripts.add(path.resolve(dir, declared[1]));
    } catch { /* no readable Cargo.toml */ }
    for (const script of scripts) {
      refs.add(script);
      let text;
      try { text = readFileSync(script, "utf8"); } catch { continue; }
      const masked = maskRust(text);
      const add = (base, rel) => {
        if (!rel || /^[a-z][a-z0-9+.-]*:\/\//i.test(rel)) return;
        const abs = path.resolve(base, rel);
        if (existsSync(abs)) refs.add(abs);
      };
      for (const m of masked.matchAll(/\binclude(?:_str|_bytes)?\s*!\s*\(/g)) {
        const close = matchingClose(masked, m.index + m[0].length - 1);
        for (const lit of quoted(text.slice(m.index, close < 0 ? text.length : close))) add(path.dirname(script), lit);
      }
      for (const m of masked.matchAll(/\.\s*(?:files?|include|flag|flag_if_supported)\s*\(/g)) {
        const close = matchingClose(masked, m.index + m[0].length - 1);
        for (const lit of quoted(text.slice(m.index, close < 0 ? text.length : close))) add(dir, lit.replace(/^-I\s*/, ""));
      }
      for (const m of text.matchAll(/cargo::?rerun-if-changed=([^"\\\n]+)/g)) add(dir, m[1].trim());
    }
  }
  return refs;
}
