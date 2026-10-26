// Shell contract: the files-only check of how an app wires rightkit-tauri-shell (rules SC-R1..SC-R9; scan gaps C1-01, C1-02, C1-03,
// C1-04 and C1-10). It reads tracked files and nothing else: no cargo, no pnpm, no network, no compile. It imports node builtins
// only, because preflight runs with no node_modules; the one piece of devtools-policy.mjs it needs, the content-devtools Cargo
// test, is a copy below (importing that module pulls in the build and managed-target code through qa-build-features.mjs).
//
// Each rule is { id, gap, run({ root, files }) } and returns findings [{ file, subject, problem, fix }] the way the G13-G21 app
// rules of scripts/preflight.mjs do: `root` is the directory that holds the exported tree and `files` the posix path of every
// tracked file in it. `problem` starts with "<file>:<line> ..." and `fix` says the exact change. The identity of a finding is
// rule|file|subject, so `subject` never carries a line number. A rule never throws on repository content: JSON that does not
// parse, Rust that is odd and files that are missing become a finding or a skip.
//
//   shell-window-hidden        SC-R1  a window created at startup that is not visible:false on some target
//   shell-plugin-order         SC-R2  the shell plugin is installed late or from setup
//   shell-handler              SC-R3  a rk_shell_* command the renderer calls that the Rust side does not register
//   shell-csp                  SC-R4  a CSP below the baseline
//   shell-grants               SC-R5  core:window:allow-* granted while the shell's window command is registered
//   qa-window-label            SC-R6  QA Control binds a window label the shell does not use
//   devtools-declaration       SC-R7  the five sites of the content-only DevTools declaration disagree
//   shell-lifecycle-external   SC-R8  lifecycle_only next to an external-URL window (no hardening, no navigation policy)
//   legal-receipt-native       SC-R9  legal acceptance stamped with the webview clock instead of the native receipt service
//
// An app is every tracked tauri.conf.json outside node_modules/, vendor/ and target/. Targets are `base` plus each tauri.<platform>.conf.json:
// Tauri merges a platform file into the base by JSON Merge Patch (RFC 7396: objects merge, arrays replace, null deletes), and a platform
// target is reported only when its own patch restates the key the rule checks (otherwise it inherits the base result).
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const SKIPPED_TREE = /(^|\/)(node_modules|vendor|target)\//;
const PLATFORMS = ["macos", "windows", "linux", "ios", "android"];
// The shell is a desktop plugin: a mobile window is created by the OS and shown by it, so R1 judges desktop targets only.
const DESKTOP = new Set(["base", "macos", "windows", "linux"]);
const SHELL_CRATES = ["rightkit-tauri-shell", "rightkit-shell"];
const ALLOW_LIST_FILE = "packages/release/devtools-policy.json";
// The npm bridge's command map (R3 maps what an app imports from @rightkit/tauri-shell to rk_shell_* commands). It ships inside this
// package as shell-contract-bridge.json, generated from packages/tauri-shell/src/index.ts by scripts/generate-shell-contract-bridge.mjs
// and held to it by a test, because an installed copy of this package has no tauri-shell source beside it.
const BRIDGE_FILE = fileURLToPath(new URL("./shell-contract-bridge.json", import.meta.url));
// The source in this checkout, only as a fallback when the JSON is absent (a damaged copy); an installed package has neither and
// maps nothing, so every @rightkit/tauri-shell import then needs at least one registered command.
const TAURI_SHELL_INDEX = fileURLToPath(new URL("../tauri-shell/src/index.ts", import.meta.url));
const POLICY_FILE = fileURLToPath(new URL("./devtools-policy.json", import.meta.url));

// ---- small helpers ---------------------------------------------------------------------------------------------------------
const readText = (root, rel) => { try { return fs.readFileSync(path.join(root, rel), "utf8").replace(/^\uFEFF/, ""); } catch { return null; } };
const parseJson = (text) => { try { return { json: JSON.parse(text) }; } catch (error) { return { error: String(error?.message ?? error) }; } };
const isObject = (v) => v !== null && typeof v === "object" && !Array.isArray(v);
const blank = (s) => s.replace(/[^\n]/g, " ");
const join = (dir, rel) => (dir ? `${dir}/${rel}` : rel);
const under = (dir, file) => dir === "" || file.startsWith(`${dir}/`);
const relTo = (dir, file) => (dir === "" ? file : file.slice(dir.length + 1));
const lineOf = (text, offset) => { let line = 1; for (let i = text.indexOf("\n"); i !== -1 && i < offset; i = text.indexOf("\n", i + 1)) line += 1; return line; };
const lineOfPattern = (text, pattern) => { const m = pattern.exec(text ?? ""); return m ? lineOf(text, m.index) : 1; };
const escapeRegExp = (s) => s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
const uniqueBy = (items, key) => { const seen = new Set(); return items.filter((item) => { const k = key(item); if (seen.has(k)) return false; seen.add(k); return true; }); };
const dedupe = (findings) => uniqueBy(findings, (f) => `${f.file}|${f.subject}`);

/** RFC 7396 JSON Merge Patch: objects merge recursively, any other patch value replaces, null deletes the key. */
export function mergePatch(target, patch) {
  if (!isObject(patch)) return patch;
  const out = isObject(target) ? { ...target } : {};
  for (const [key, value] of Object.entries(patch)) {
    if (key === "__proto__") continue;
    if (value === null) delete out[key];
    else out[key] = mergePatch(out[key], value);
  }
  return out;
}

const hasPath = (value, keys) => keys.every((key) => { if (!isObject(value) || !Object.hasOwn(value, key)) return false; value = value[key]; return true; });
const getPath = (value, keys) => keys.reduce((v, key) => (isObject(v) ? v[key] : undefined), value);

// ---- Rust, read without a parser --------------------------------------------------------------------------------------------
// scanRust returns the text twice with every comment blanked (offsets and line numbers kept): `code` keeps string and char literals,
// `skel` blanks their contents too, so brackets and keywords found in `skel` are real code. `#[cfg(test)]` items are blanked in both.
const isIdentChar = (ch) => ch !== undefined && /[A-Za-z0-9_]/.test(ch);

/** The index of the bracket that closes the one at `open` in `skel`, or -1. */
export function matchClose(skel, open) {
  const o = skel[open];
  const c = o === "(" ? ")" : o === "[" ? "]" : o === "{" ? "}" : null;
  if (c === null) return -1;
  let depth = 0;
  for (let i = open; i < skel.length; i++) {
    const ch = skel[i];
    if (ch === o) depth += 1;
    else if (ch === c && --depth === 0) return i;
  }
  return -1;
}

/** The top-level arguments of the bracket group opened at `open`, as trimmed [start, end) ranges (a trailing comma adds none). */
export function callArgs(skel, open) {
  const close = matchClose(skel, open);
  if (close < 0) return [];
  const ranges = [];
  let depth = 0;
  let start = open + 1;
  const push = (end) => {
    let s = start; let e = end;
    while (s < e && /\s/.test(skel[s])) s += 1;
    while (e > s && /\s/.test(skel[e - 1])) e -= 1;
    if (e > s) ranges.push([s, e]);
  };
  for (let i = open + 1; i < close; i++) {
    const ch = skel[i];
    if (ch === "(" || ch === "[" || ch === "{") depth += 1;
    else if (ch === ")" || ch === "]" || ch === "}") depth -= 1;
    else if (ch === "," && depth === 0) { push(i); start = i + 1; }
  }
  push(close);
  return ranges;
}

export function scanRust(text) {
  const n = text.length;
  const code = [];
  const skel = [];
  let run = 0;
  const flush = (to) => { if (to > run) { const s = text.slice(run, to); code.push(s); skel.push(s); } };
  const comment = (from, to) => { flush(from); const b = blank(text.slice(from, to)); code.push(b); skel.push(b); run = to; };
  const literal = (from, bodyFrom, bodyTo, to) => { flush(from); code.push(text.slice(from, to)); skel.push(text.slice(from, bodyFrom) + blank(text.slice(bodyFrom, bodyTo)) + text.slice(bodyTo, to)); run = to; };
  let i = 0;
  while (i < n) {
    const ch = text[i];
    if (ch === "/" && text[i + 1] === "/") { const j = text.indexOf("\n", i); const end = j < 0 ? n : j; comment(i, end); i = end; continue; }
    if (ch === "/" && text[i + 1] === "*") {
      let depth = 1; let j = i + 2;
      while (j < n && depth > 0) { if (text[j] === "/" && text[j + 1] === "*") { depth += 1; j += 2; } else if (text[j] === "*" && text[j + 1] === "/") { depth -= 1; j += 2; } else j += 1; }
      comment(i, j); i = j; continue;
    }
    if (ch === '"') {
      let j = i + 1;
      while (j < n && text[j] !== '"') j += text[j] === "\\" ? 2 : 1;
      const end = Math.min(j + 1, n);
      literal(i, i + 1, Math.min(j, n), end); i = end; continue;
    }
    if (ch === "r" && (!isIdentChar(text[i - 1]) || (text[i - 1] === "b" && !isIdentChar(text[i - 2])))) {
      let j = i + 1; let hashes = 0;
      while (text[j] === "#") { hashes += 1; j += 1; }
      if (text[j] === '"') {
        const close = `"${"#".repeat(hashes)}`;
        const at = text.indexOf(close, j + 1);
        const bodyTo = at < 0 ? n : at;
        const end = at < 0 ? n : at + close.length;
        literal(i, j + 1, bodyTo, end); i = end; continue;
      }
    }
    if (ch === "'") {
      let end = -1;
      if (text[i + 1] === "\\") { const j = text.indexOf("'", i + 3); if (j > 0 && j - i <= 12) end = j + 1; }
      else if (text[i + 1] !== undefined && text[i + 1] !== "'") { const width = text.codePointAt(i + 1) > 0xffff ? 2 : 1; if (text[i + 1 + width] === "'") end = i + 2 + width; }
      if (end > 0) { literal(i, i + 1, end - 1, end); i = end; continue; }
    }
    i += 1;
  }
  flush(n);
  let c = code.join("");
  let s = skel.join("");
  // `#![cfg(test)]` makes the whole file test code; `#[cfg(test)] item` is blanked item by item.
  const cfgTest = /#\s*(!?)\s*\[\s*cfg\s*\(\s*test\s*\)\s*\]/g;
  const ranges = [];
  for (let m = cfgTest.exec(s); m; m = cfgTest.exec(s)) {
    if (m[1] === "!") return { code: blank(c), skel: blank(s) };
    let j = m.index + m[0].length;
    for (;;) {
      while (/\s/.test(s[j] ?? "")) j += 1;
      if (s[j] === "#" && /^#\s*\[/.test(s.slice(j, j + 8))) { const open = s.indexOf("[", j); const close = matchClose(s, open); if (close < 0) break; j = close + 1; } else break;
    }
    // Only an item is blanked; a test-only match arm, field or statement stays (it holds no registration worth reading).
    if (!/^(?:pub(?:\([^)]*\))?\s+)?(?:(?:async|unsafe|const|extern(?:\s+"[^"]*")?)\s+)*(?:mod|fn|use|impl|struct|enum|trait|type|static|const|macro_rules\s*!)\b/.test(s.slice(j, j + 120))) continue;
    let depth = 0; let end = s.length;
    for (let k = j; k < s.length; k++) {
      const ck = s[k];
      if (ck === "(" || ck === "[") depth += 1;
      else if (ck === ")" || ck === "]") depth -= 1;
      else if (depth === 0 && ck === ";") { end = k + 1; break; }
      else if (depth === 0 && ck === "{") { const close = matchClose(s, k); end = close < 0 ? s.length : close + 1; break; }
    }
    ranges.push([m.index, end]);
    cfgTest.lastIndex = Math.max(cfgTest.lastIndex, end);
  }
  for (const [from, to] of ranges.reverse()) { c = c.slice(0, from) + blank(c.slice(from, to)) + c.slice(to); s = s.slice(0, from) + blank(s.slice(from, to)) + s.slice(to); }
  return { code: c, skel: s };
}

const RUST_CACHE = new Map();
const parsedRust = (text) => {
  let parsed = RUST_CACHE.get(text);
  if (!parsed) { if (RUST_CACHE.size > 4000) RUST_CACHE.clear(); parsed = scanRust(text); RUST_CACHE.set(text, parsed); }
  return parsed;
};

/** Every `fn name(..) .. { body }` of the skeleton: { name, start, bodyStart, bodyEnd } (declarations without a body are skipped). */
export function rustFns(skel) {
  const fns = [];
  const FN = /\bfn\s+([A-Za-z_]\w*)/g;
  for (let m = FN.exec(skel); m; m = FN.exec(skel)) {
    let i = m.index + m[0].length;
    while (/\s/.test(skel[i] ?? "")) i += 1;
    if (skel[i] === "<") {
      let depth = 0;
      for (; i < skel.length; i++) { if (skel[i] === "<") depth += 1; else if (skel[i] === ">" && skel[i - 1] !== "-" && skel[i - 1] !== "=" && --depth === 0) { i += 1; break; } }
    }
    while (/\s/.test(skel[i] ?? "")) i += 1;
    if (skel[i] !== "(") continue;
    const paramsEnd = matchClose(skel, i);
    if (paramsEnd < 0) continue;
    let depth = 0; let bodyStart = -1;
    for (let k = paramsEnd + 1; k < skel.length; k++) {
      const ck = skel[k];
      if (ck === "(" || ck === "[") depth += 1;
      else if (ck === ")" || ck === "]") depth -= 1;
      else if (depth === 0 && ck === ";") break;
      else if (depth === 0 && ck === "{") { bodyStart = k; break; }
    }
    if (bodyStart < 0) continue;
    const bodyEnd = matchClose(skel, bodyStart);
    if (bodyEnd > 0) fns.push({ name: m[1], start: m.index, bodyStart, bodyEnd });
  }
  return fns;
}
const innermostFn = (fns, offset) => fns.filter((f) => f.bodyStart <= offset && offset <= f.bodyEnd).sort((a, b) => b.bodyStart - a.bodyStart)[0];

const SHELL_CONSTRUCT = /\b(?:Shell|ShellPlugin)\s*::\s*builder\s*\(/;
const squash = (s) => s.replace(/\s+/g, " ").trim();
/** `const NAME: &str = "value";` (or static) literals of a Rust text: Map name -> value. */
function stringConsts(parsedFiles) {
  const consts = new Map();
  for (const { code } of parsedFiles) for (const m of code.matchAll(/\b(?:const|static)\s+([A-Z][A-Z0-9_]*)\s*:\s*&(?:'static\s+)?str\s*=\s*"([^"\\]*)"\s*;/g)) consts.set(m[1], m[2]);
  return consts;
}
/** The label a Rust argument names: a "literal", or an identifier that is a string const; undefined when it is neither. */
function labelOf(argCode, consts) {
  const lit = /^&?\s*"([^"\\]*)"$/.exec(argCode);
  if (lit) return lit[1];
  const name = /^(?:[A-Za-z_]\w*::)*([A-Z][A-Z0-9_]*)$/.exec(argCode.trim());
  return name ? consts.get(name[1]) : undefined;
}

// ---- JavaScript and JSX, read without a parser -------------------------------------------------------------------------------
function skipQuoted(text, i, quote) {
  let j = i + 1;
  while (j < text.length && text[j] !== quote && text[j] !== "\n") j += text[j] === "\\" ? 2 : 1;
  return Math.min(j + 1, text.length);
}
// A `/` starts a regular expression, not a division, after one of these tokens (`<` is left out: `</div>` is JSX).
const REGEX_AFTER = /[(,=:[!&|?{;+\-*%~^]$|\b(?:return|typeof|case|in|of|delete|void|throw|new|else|do)$/;
// A quote starts a string in expression position; after JSX text (`Don't`) it is only an apostrophe.
const QUOTE_AFTER = /[(,=:[!&|?{};+\-*%<>~^]$|\b(?:return|typeof|case|in|of|delete|void|throw|new|else|do|from|import|export|default|extends|yield|await|as)$/;
const quoteAllowed = (text, i) => { const before = text.slice(Math.max(0, i - 12), i).trimEnd(); return before === "" || QUOTE_AFTER.test(before); };
const regexAllowed = (text, i) => { const before = text.slice(Math.max(0, i - 12), i).trimEnd(); return before === "" || REGEX_AFTER.test(before); };
function skipRegex(text, i) {
  let j = i + 1; let inClass = false;
  while (j < text.length && text[j] !== "\n") {
    if (text[j] === "\\") { j += 2; continue; }
    if (text[j] === "[") inClass = true; else if (text[j] === "]") inClass = false; else if (text[j] === "/" && !inClass) { j += 1; while (/[a-z]/i.test(text[j] ?? "")) j += 1; return j; }
    j += 1;
  }
  return i + 1;
}
function skipTemplate(text, i) {
  let j = i + 1;
  while (j < text.length && text[j] !== "`") {
    if (text[j] === "\\") { j += 2; continue; }
    if (text[j] === "$" && text[j + 1] === "{") {
      let depth = 1; j += 2;
      while (j < text.length && depth > 0) {
        if (text[j] === "`") j = skipTemplate(text, j);
        else if (text[j] === '"' || text[j] === "'") j = skipQuoted(text, j, text[j]);
        else { if (text[j] === "{") depth += 1; else if (text[j] === "}") depth -= 1; j += 1; }
      }
      continue;
    }
    j += 1;
  }
  return Math.min(j + 1, text.length);
}

/** `code`: comments blanked. `skel`: comments blanked and string and template contents blanked too (delimiters kept). */
export function scanJs(text) {
  const code = []; const skel = [];
  let run = 0;
  const flush = (to) => { if (to > run) { const s = text.slice(run, to); code.push(s); skel.push(s); } };
  let i = 0;
  while (i < text.length) {
    const ch = text[i];
    if (ch === "/" && (text[i + 1] === "/" || text[i + 1] === "*")) {
      flush(i);
      let end;
      if (text[i + 1] === "/") { const j = text.indexOf("\n", i); end = j < 0 ? text.length : j; } else { const j = text.indexOf("*/", i + 2); end = j < 0 ? text.length : j + 2; }
      const b = blank(text.slice(i, end)); code.push(b); skel.push(b); run = i = end; continue;
    }
    if (ch === "`" || ((ch === '"' || ch === "'") && quoteAllowed(text, i)) || (ch === "/" && regexAllowed(text, i))) {
      flush(i);
      const end = ch === "`" ? skipTemplate(text, i) : ch === "/" ? skipRegex(text, i) : skipQuoted(text, i, ch);
      code.push(text.slice(i, end));
      skel.push(ch + blank(text.slice(i + 1, Math.max(i + 1, end - 1))) + (end - i >= 2 ? text[end - 1] : ""));
      run = i = end; continue;
    }
    i += 1;
  }
  flush(text.length);
  return { code: code.join(""), skel: skel.join("") };
}
const JS_CACHE = new Map();
const parsedJs = (text) => {
  let parsed = JS_CACHE.get(text);
  if (!parsed) { if (JS_CACHE.size > 4000) JS_CACHE.clear(); parsed = scanJs(text); JS_CACHE.set(text, parsed); }
  return parsed;
};

/** The attributes of the JSX element that opens at `lt` (a `<` in `skel`): { attrs: Map name -> { kind, text, offset }, spread, end } or null. */
export function readJsxTag(skel, code, lt) {
  let i = lt + 1;
  while (/[\w.$]/.test(skel[i] ?? "")) i += 1;
  const attrs = new Map();
  let spread = false;
  const ATTR = /[A-Za-z_$][\w$:-]*/y;
  for (;;) {
    while (/\s/.test(skel[i] ?? "")) i += 1;
    if (i >= skel.length) return null;
    if (skel[i] === "/" && skel[i + 1] === ">") return { attrs, spread, end: i + 2 };
    if (skel[i] === ">") return { attrs, spread, end: i + 1 };
    if (skel[i] === "{") { const close = matchClose(skel, i); if (close < 0) return null; spread = true; i = close + 1; continue; }
    ATTR.lastIndex = i;
    const name = ATTR.exec(skel)?.[0];
    if (!name) return null;
    i += name.length;
    while (/\s/.test(skel[i] ?? "")) i += 1;
    if (skel[i] !== "=") { attrs.set(name, { kind: "true", text: "true", offset: i }); continue; }
    i += 1;
    while (/\s/.test(skel[i] ?? "")) i += 1;
    if (skel[i] === "{") { const close = matchClose(skel, i); if (close < 0) return null; attrs.set(name, { kind: "expr", text: code.slice(i + 1, close).trim(), offset: i }); i = close + 1; }
    else if (skel[i] === '"' || skel[i] === "'") { const close = skel.indexOf(skel[i], i + 1); if (close < 0) return null; attrs.set(name, { kind: "string", text: code.slice(i + 1, close), offset: i }); i = close + 1; }
    else return null;
  }
}

// ---- apps -------------------------------------------------------------------------------------------------------------------
const RUST_SKIP = /(^|\/)(target|gen|tests|benches)\//;
const RUST_TEST_FILE = /(^|\/)(tests?|[^/]*_tests?)\.rs$/;
const RENDERER_SKIP = /(^|\/)(node_modules|dist|build|src-tauri|target|__tests__|tests|e2e|scripts)\/|\.(test|spec)\.[^/]*$|\.d\.ts$/;
const RENDERER_FILE = /\.(ts|tsx|js|jsx|mjs)$/;

/**
 * Every app under `files`: one per tracked tauri.conf.json outside node_modules/, vendor/ and target/.
 * { confDir, appRoot, conf, platforms, targets(keyPath), capabilityFiles, rustFiles, rendererFiles, releaseConfig, cargoToml, buildRs }
 * (directories are posix paths relative to the tree, "" for the root; a file that does not parse carries `error`).
 */
export function discoverApps({ root, files }) {
  const tracked = new Set(files);
  const confFiles = files.filter((f) => (f === "tauri.conf.json" || f.endsWith("/tauri.conf.json")) && !SKIPPED_TREE.test(f)).sort();
  const confDirs = confFiles.map((f) => f.slice(0, -"tauri.conf.json".length).replace(/\/$/, ""));
  const rootOf = (confDir) => (path.posix.basename(confDir) === "src-tauri" ? (path.posix.dirname(confDir) === "." ? "" : path.posix.dirname(confDir)) : confDir);
  const appRoots = confDirs.map(rootOf);
  const apps = [];
  confFiles.forEach((confFile, index) => {
    const confDir = confDirs[index];
    const appRoot = rootOf(confDir);
    const read = (file, target) => { const text = readText(root, file); return { file, target, text, ...(text === null ? { error: "cannot be read" } : parseJson(text)) }; };
    const conf = read(confFile, "base");
    const platforms = PLATFORMS.map((p) => join(confDir, `tauri.${p}.conf.json`)).filter((f) => tracked.has(f)).map((f) => read(f, path.posix.basename(f).split(".")[1]));
    // Files of a nested app belong to that app: a file under a deeper app's directory is not this app's.
    const owned = (dir, file, dirs) => !dirs.some((other) => other.length > dir.length && under(other, file) && under(dir, other));
    const rustFiles = files.filter((f) => f.endsWith(".rs") && under(confDir, f) && !SKIPPED_TREE.test(f) && !RUST_SKIP.test(relTo(confDir, f)) && !RUST_TEST_FILE.test(f) && owned(confDir, f, confDirs));
    const rendererFiles = files.filter((f) => RENDERER_FILE.test(f) && under(appRoot, f) && !SKIPPED_TREE.test(f) && !RENDERER_SKIP.test(relTo(appRoot, f)) && owned(appRoot, f, appRoots));
    const capabilityFiles = files.filter((f) => f.endsWith(".json") && under(join(confDir, "capabilities"), f) && !SKIPPED_TREE.test(f));
    let releaseConfig = null;
    for (let dir = appRoot; releaseConfig === null; dir = path.posix.dirname(dir) === "." ? "" : path.posix.dirname(dir)) {
      releaseConfig = ["right-release.config.json", "right-release.config.mjs"].map((n) => join(dir, n)).find((f) => tracked.has(f)) ?? null;
      if (dir === "") break;
    }
    apps.push({
      confDir, appRoot, conf, platforms, rustFiles, rendererFiles, capabilityFiles, releaseConfig,
      cargoToml: tracked.has(join(confDir, "Cargo.toml")) ? join(confDir, "Cargo.toml") : null,
      buildRs: tracked.has(join(confDir, "build.rs")) ? join(confDir, "build.rs") : null,
      /** The merged config of `base` and of each platform whose own patch restates `keyPath` (all of them when keyPath is empty). */
      targets(keyPath = []) {
        if (conf.error || !isObject(conf.json)) return [];
        const out = [{ name: "base", file: confFile, merged: conf.json }];
        for (const p of platforms) if (!p.error && isObject(p.json) && (keyPath.length === 0 || hasPath(p.json, keyPath))) out.push({ name: p.target, file: p.file, merged: mergePatch(conf.json, p.json) });
        return out;
      },
    });
  });
  return apps;
}

const readRust = (root, app, needle) => app.rustFiles.map((file) => ({ file, text: readText(root, file) })).filter((f) => f.text !== null && (needle === undefined || needle.test(f.text))).map((f) => ({ ...f, ...parsedRust(f.text) }));
const readRenderer = (root, app, needle) => app.rendererFiles.map((file) => ({ file, text: readText(root, file) })).filter((f) => f.text !== null && (needle === undefined || needle.test(f.text))).map((f) => ({ ...f, ...parsedJs(f.text) }));

// Every rule judges one app at a time; a rule that fails on one app reports that as a finding with an identity (never a throw, which
// preflight can only show as an unkeyed FAIL that no baseline can list) and still judges the rest.
const ruleError = (app, error) => ({ file: app.conf.file,
  subject: "rule-error",
  problem: `${app.conf.file}:1 the shell-contract rule failed on this app (${String(error?.message ?? error).slice(0, 120)})`,
  fix: "report it to rightkit with this tauri.conf.json: a rule must not fail on repository content",
});
const perApp = (check) => (input) => {
  const out = [];
  for (const app of discoverApps(input)) { try { out.push(...check({ ...input, app })); } catch (error) { out.push(ruleError(app, error)); } }
  return dedupe(out);
};

// ---- R1 shell-window-hidden --------------------------------------------------------------------------------------------------
function conflictFindings(app) {
  return [app.conf, ...app.platforms].filter((c) => c.error).map((c) => ({ file: c.file, subject: `${c.target}:unparseable`, problem: `${c.file}:1 is not valid JSON (${c.error}), so no shell-contract rule can read this ${c.target} config`, fix: "fix the JSON syntax; Tauri refuses the file too" }));
}

const shellWindowHidden = perApp(({ app, root }) => {
  const out = conflictFindings(app);
  for (const target of app.targets(["app", "windows"])) {
    if (!DESKTOP.has(target.name)) continue;
    const windows = getPath(target.merged, ["app", "windows"]);
    if (!Array.isArray(windows)) continue;
    const text = readText(root, target.file) ?? "";
    windows.forEach((w, i) => {
      if (!isObject(w) || w.create === false || w.visible === false) return;
      const label = typeof w.label === "string" ? w.label : "main";
      const line = lineOfPattern(text, new RegExp(`"label"\\s*:\\s*"${escapeRegExp(label)}"`));
      const restated = target.name === "base" ? "" : ` This ${target.name} file restates app.windows, and Tauri replaces arrays on merge, so it has to say visible: false itself.`;
      out.push({
        file: target.file,
        subject: `${target.name}:${label}${windows.filter((x, j) => j < i && (x?.label ?? "main") === label).length ? `#${i}` : ""}`,
        problem: `${target.file}:${line} window "${label}" is created at startup on ${target.name} without "visible": false: the shell shows the window when its webview is ready, so a window that starts visible flashes and races show-on-ready.${restated}`,
        fix: `set "visible": false on window "${label}" in ${target.file} (an app that owns its windows, with host_windows or lifecycle_only, shows it itself once its webview is ready)`,
      });
    });
  }
  return out;
});

// ---- R4 shell-csp ------------------------------------------------------------------------------------------------------------
/** The directives of a Tauri `app.security.csp` (a string or a directive object): Map name -> [source...], or null when it has none. */
export function parseCsp(csp) {
  const directives = new Map();
  if (typeof csp === "string") {
    for (const part of csp.split(";")) { const tokens = part.trim().split(/\s+/).filter(Boolean); if (tokens.length) { const name = tokens.shift().toLowerCase(); if (!directives.has(name)) directives.set(name, tokens); } }
  } else if (isObject(csp)) {
    for (const [name, value] of Object.entries(csp)) directives.set(name.toLowerCase(), (Array.isArray(value) ? value : [value]).flatMap((v) => String(v).split(/\s+/)).filter(Boolean));
  }
  return directives.size ? directives : null;
}
const hasToken = (tokens, token) => (tokens ?? []).some((t) => t.toLowerCase() === token);

const shellCsp = perApp(({ app, root }) => {
  const out = [];
  for (const target of app.targets(["app", "security", "csp"])) {
    const text = readText(root, target.file) ?? "";
    const line = lineOfPattern(text, /"csp"\s*:/);
    const csp = parseCsp(getPath(target.merged, ["app", "security", "csp"]));
    const add = (what, why, fix) => out.push({ file: target.file, subject: `${target.name}:${what}`, problem: `${target.file}:${line} ${target.name} CSP ${why}`, fix });
    if (!csp) { add("csp-missing", "is missing: the webview runs with no content security policy at all", `set app.security.csp in ${target.file} (default-src 'self'; frame-ancestors 'none'; connect-src with the hosts the app calls)`); continue; }
    if (!hasToken(csp.get("default-src"), "'self'")) add("default-src", "has no default-src 'self'", "add default-src 'self' to the policy");
    if (!hasToken(csp.get("frame-ancestors"), "'none'")) add("frame-ancestors", "has no frame-ancestors 'none': the page can be framed by another origin", "add frame-ancestors 'none' to the policy");
    const scripts = csp.get("script-src") ?? csp.get("default-src");
    const scriptName = csp.has("script-src") ? "script-src" : "default-src (no script-src)";
    if (hasToken(scripts, "'unsafe-inline'")) add("script-src-unsafe-inline", `allows 'unsafe-inline' in ${scriptName}: any injected markup can run script`, "remove 'unsafe-inline' from script-src (style-src may keep it)");
    if (hasToken(scripts, "'unsafe-eval'")) add("script-src-unsafe-eval", `allows 'unsafe-eval' in ${scriptName}: strings can be run as code`, "remove 'unsafe-eval' from script-src");
    if (!csp.has("connect-src")) add("connect-src", "has no connect-src: network access falls back to default-src and the hosts the app calls are not declared", "add connect-src 'self' plus the hosts the app calls (ipc: is advisory)");
  }
  return out;
});

// ---- shell registration in Rust (R2, R3, R5, R6, R8) --------------------------------------------------------------------------
/** The shell handler of an app's Rust: whether handler![..] is used, each hand-listed rk_shell_* command, and where generate_handler! sits. */
function handlerInfo(parsedFiles) {
  const info = { macro: null, listed: new Map(), sites: [] };
  for (const { file, text, code, skel } of parsedFiles) {
    const macro = /\b(?:rightkit_tauri_shell|rightkit_shell)\s*::\s*handler\s*!\s*[[({]/.exec(skel);
    if (macro && !info.macro) info.macro = { file, line: lineOf(text, macro.index) };
    for (const m of skel.matchAll(/\bgenerate_handler\s*!\s*([[({])/g)) {
      const open = m.index + m[0].length - 1;
      info.sites.push({ file, line: lineOf(text, m.index) });
      for (const [s, e] of callArgs(skel, open)) {
        const name = /([A-Za-z_]\w*)\s*$/.exec(code.slice(s, e))?.[1];
        if (name?.startsWith("rk_shell_") && !info.listed.has(name)) info.listed.set(name, { file, line: lineOf(text, s) });
      }
    }
  }
  return info;
}
const registers = (info, command) => info.macro !== null || info.listed.has(command);

// ---- R2 shell-plugin-order ---------------------------------------------------------------------------------------------------
const QA_PLUGIN = /rightkit_qa|control|(^|[^a-z])qa([^a-z]|$)|webdriver|wdio/i;

const shellPluginOrder = perApp(({ app, root }) => {
  const out = [];
  const parsed = readRust(root, app, /\.\s*plugin\s*\(|\b(?:Shell|ShellPlugin)\s*::\s*builder/).map((f) => ({ ...f, fns: rustFns(f.skel) }));
  // Functions that build a shell: directly, or by calling one that does.
  const shellFns = [];
  for (const f of parsed) for (const fn of f.fns) if (SHELL_CONSTRUCT.test(f.skel.slice(fn.bodyStart, fn.bodyEnd))) shellFns.push({ ...fn, file: f.file });
  for (let round = 0; round < 2; round++) {
    for (const f of parsed) for (const fn of f.fns) {
      if (shellFns.some((s) => s.file === f.file && s.start === fn.start)) continue;
      const body = f.skel.slice(fn.bodyStart, fn.bodyEnd);
      if (shellFns.some((s) => new RegExp(`\\b${s.name}\\s*\\(`).test(body))) shellFns.push({ ...fn, file: f.file });
    }
  }
  const stem = (file) => { const base = path.posix.basename(file, ".rs"); return base === "mod" ? path.posix.basename(path.posix.dirname(file)) : base; };
  const callsShellFn = (arg, file) => {
    const m = /^\s*&?\s*((?:[A-Za-z_]\w*\s*::\s*)*[A-Za-z_]\w*)\s*\(/.exec(arg);
    if (!m) return false;
    const parts = m[1].split(/\s*::\s*/);
    const name = parts.at(-1);
    const qualifier = parts.length > 1 ? parts.at(-2) : null;
    let candidates = shellFns.filter((s) => s.name === name);
    if (qualifier && !["crate", "self", "super"].includes(qualifier)) candidates = candidates.filter((s) => stem(s.file) === qualifier);
    else if (!qualifier && candidates.some((s) => s.file === file)) candidates = candidates.filter((s) => s.file === file);
    return candidates.length > 0;
  };
  for (const f of parsed) {
    const fns = f.fns;
    const setups = [...f.skel.matchAll(/\.\s*setup\s*\(/g)].map((m) => callArgs(f.skel, m.index + m[0].length - 1)[0]).filter(Boolean);
    const regs = [];
    for (const m of f.skel.matchAll(/\.\s*plugin\s*\(/g)) {
      const [arg] = callArgs(f.skel, m.index + m[0].length - 1);
      if (!arg) continue;
      const argCode = f.code.slice(arg[0], arg[1]);
      const fn = innermostFn(fns, m.index);
      const scope = fn ? f.skel.slice(fn.bodyStart, m.index) : f.skel.slice(0, m.index);
      const scopeCode = fn ? f.code.slice(fn.bodyStart, m.index) : f.code.slice(0, m.index);
      let shell = SHELL_CONSTRUCT.test(argCode) || callsShellFn(argCode, f.file);
      const variable = /^\s*&?\s*([A-Za-z_]\w*)\s*\./.exec(argCode)?.[1];
      if (!shell && variable) {
        for (const b of scope.matchAll(new RegExp(`\\blet\\s+(?:mut\\s+)?${variable}\\b[^;=]*=`, "g"))) {
          const end = scope.indexOf(";", b.index);
          if (SHELL_CONSTRUCT.test(scope.slice(b.index, end < 0 ? scope.length : end))) shell = true;
        }
      }
      // An identifier argument is QA when what it was bound from is (`if let Some(plugin) = control.build_if_enabled()`).
      let qa = QA_PLUGIN.test(argCode);
      if (!qa && /^[A-Za-z_]\w*$/.test(argCode)) {
        for (const b of scopeCode.matchAll(new RegExp(`(?:\\blet\\s+(?:mut\\s+)?${argCode}\\b[^;=]*|Some\\(\\s*${argCode}\\s*\\))\\s*=\\s*([^;{]*)`, "g"))) if (QA_PLUGIN.test(b[1])) qa = true;
      }
      regs.push({ at: m.index, argCode, fn, shell, qa, single: /single_instance_plugin/.test(argCode), inSetup: setups.some(([s, e]) => m.index >= s && m.index < e) });
    }
    for (const s of regs.filter((r) => r.shell)) {
      if (s.inSetup) {
        out.push({
          file: f.file,
          subject: "setup-install",
          problem: `${f.file}:${lineOf(f.text, s.at)} installs the shell plugin from setup (.plugin(..) inside .setup(..)): the window, the single-instance claim and the other plugins have already started by then`,
          fix: "register it on the builder, tauri::Builder::default().plugin(shell.plugin()), before every non-QA plugin, and leave setup for work that needs the running app",
        });
        continue;
      }
      // A shell built with single_instance(false) leaves arbitration to another plugin, which has to come first (CodeRight's
      // desktop_shell::plugin); the order then guards nothing the shell does, so only the setup-time install is a finding.
      if (parsed.some((p) => (p.file === f.file || shellFns.some((fn) => fn.file === p.file)) && /\.\s*single_instance\s*\(\s*false\s*\)/.test(p.skel))) continue;
      for (const r of regs.filter((x) => !x.shell && !x.inSetup && x.at < s.at && x.fn === s.fn && !x.qa && !x.single)) {
        const head = squash(/^[^(]*/.exec(r.argCode)[0]) || r.argCode.slice(0, 40);
        out.push({
          file: f.file,
          subject: `before:${head}`,
          problem: `${f.file}:${lineOf(f.text, r.at)} registers ${head} before the shell plugin (${f.file}:${lineOf(f.text, s.at)}): the shell's single-instance claim, hardening and window handling must run before any other plugin does work`,
          fix: `move the shell plugin ahead of ${head} (only the shell's single_instance_plugin and QA-only plugins may come first)`,
        });
      }
    }
  }
  return out;
});

// ---- R3 shell-handler --------------------------------------------------------------------------------------------------------
// A command-by-command map of the npm bridge: exported name -> the rk_shell_* literals in its body. createShell wraps every command, so
// its commands are the members an app reaches (shellMembers, shellUsage); any other export that maps to more than this many, or that the
// map does not know, counts as "needs at least one".
const FACADE_COMMANDS = 8;
/** The end of the statement that starts at `from` in a JS skeleton: the `;` (or, with no semicolon, the line break) that is not inside a bracket. */
function statementEnd(skel, from) {
  let depth = 0;
  for (let k = from; k < skel.length; k++) {
    const c = skel[k];
    if (c === "(" || c === "[" || c === "{") depth += 1;
    else if (c === ")" || c === "]" || c === "}") { depth -= 1; if (depth < 0) return k; }
    else if (depth === 0 && c === ";") return k;
    else if (depth === 0 && c === "\n" && skel.slice(from, k).trim() && !/^\s*[.?:+\-*/|&,=(\[]/.test(skel.slice(k + 1, k + 40))) return k;
  }
  return skel.length;
}

/** Exported function and const names of @rightkit/tauri-shell's index.ts with the rk_shell_* literals in each body. */
export function shellExports(indexText) {
  const map = new Map();
  if (typeof indexText !== "string") return map;
  const { code, skel } = parsedJs(indexText);
  for (const m of skel.matchAll(/\bexport\s+(?:async\s+)?(?:function\s*\*?\s+([A-Za-z_$][\w$]*)|const\s+([A-Za-z_$][\w$]*))/g)) {
    let end;
    if (m[1]) {
      let i = m.index + m[0].length;
      if (skel[i] === "<") { for (let depth = 0; i < skel.length; i++) { if (skel[i] === "<") depth += 1; else if (skel[i] === ">" && skel[i - 1] !== "=" && --depth === 0) { i += 1; break; } } }
      const params = skel.indexOf("(", i) >= 0 ? matchClose(skel, skel.indexOf("(", i)) : -1;
      const open = params < 0 ? -1 : skel.indexOf("{", params);
      end = open < 0 ? m.index + m[0].length : (matchClose(skel, open) + 1) || skel.length;
    } else end = statementEnd(skel, m.index + m[0].length);
    map.set(m[1] ?? m[2], [...new Set([...code.slice(m.index, end).matchAll(/['"`](rk_shell_[a-z0-9_]+)['"`]/g)].map((x) => x[1]))]);
  }
  return map;
}
/**
 * createShell's API as member paths with the rk_shell_* literals in each member: "window.minimize" -> ["rk_shell_window"], "quit" ->
 * ["rk_shell_quit"], "engine.status" -> ["rk_shell_engine_status"]. Read from the `const api = { .. }` of createShell in index.ts.
 */
export function shellMembers(indexText) {
  const members = new Map();
  if (typeof indexText !== "string") return members;
  const { code, skel } = parsedJs(indexText);
  const fn = /\bfunction\s+createShell\b/.exec(skel);
  const params = fn ? skel.indexOf("(", fn.index) : -1;
  const open = params < 0 ? -1 : skel.indexOf("{", matchClose(skel, params));
  const end = open < 0 ? -1 : matchClose(skel, open);
  const api = end < 0 ? null : /\bconst\s+api\s*=\s*\{/.exec(skel.slice(open, end));
  if (!api) return members;
  // `invoke(SURFACE_COMMAND, ..)`: a command named through an exported string constant.
  const consts = new Map([...code.matchAll(/\bexport\s+const\s+([A-Z][A-Z0-9_]*)\s*=\s*(['"`])(rk_shell_[a-z0-9_]+)\2/g)].map((m) => [m[1], m[3]]));
  const commandsIn = (text) => [...new Set([...[...text.matchAll(/['"`](rk_shell_[a-z0-9_]+)['"`]/g)].map((x) => x[1]), ...(text.match(/\b[A-Z][A-Z0-9_]*\b/g) ?? []).filter((id) => consts.has(id)).map((id) => consts.get(id))])];
  const walk = (objectOpen, prefix) => {
    for (const [s, e] of callArgs(skel, objectOpen)) {
      const entry = /^(?:async\s+)?([A-Za-z_$][\w$]*)\s*(:|\(|$)/.exec(skel.slice(s, e));
      if (!entry) continue;
      const path = prefix ? `${prefix}.${entry[1]}` : entry[1];
      const value = entry[2] === ":" ? s + skel.slice(s, e).indexOf(":") + 1 : -1;
      const valueAt = value < 0 ? -1 : value + skel.slice(value, e).search(/\S/);
      if (valueAt > 0 && skel[valueAt] === "{") walk(valueAt, path);
      else members.set(path, commandsIn(code.slice(s, e)));
    }
  };
  walk(open + api.index + api[0].length - 1, "");
  // shell.bridge() hands the host the commands its window, link and theme members reach, some of them through api.<member>.
  const bridge = /\bconst\s+bridge\s*=/.exec(skel.slice(open, end));
  if (bridge) {
    const from = open + bridge.index;
    const text = code.slice(from, from + statementEnd(skel.slice(from), 0));
    const commands = new Set(commandsIn(text));
    for (const ref of text.matchAll(/\bapi\.([A-Za-z_$][\w$]*)(?:\.([A-Za-z_$][\w$]*))?/g)) for (const c of members.get([ref[1], ref[2]].filter(Boolean).join(".")) ?? members.get(ref[1]) ?? []) commands.add(c);
    members.set("bridge", [...commands]);
  }
  return members;
}

/** The bridge map as the plain object the package ships: { schema, exports: { name: [command] }, members: { "window.close": [command] } }. */
export function bridgeMap(indexText) {
  return { schema: 1, exports: Object.fromEntries(shellExports(indexText)), members: Object.fromEntries(shellMembers(indexText)) };
}
const readIndex = () => { try { return fs.readFileSync(TAURI_SHELL_INDEX, "utf8"); } catch { return null; } };
const mapOf = (value) => new Map(Object.entries(isObject(value) ? value : {}).filter(([, commands]) => Array.isArray(commands) && commands.every((c) => typeof c === "string")));
let shellBridgeCache;
const defaultShellBridge = () => {
  if (shellBridgeCache) return shellBridgeCache;
  try {
    const shipped = JSON.parse(fs.readFileSync(BRIDGE_FILE, "utf8"));
    if (isObject(shipped) && shipped.schema === 1) return (shellBridgeCache = { exports: mapOf(shipped.exports), members: mapOf(shipped.members) });
  } catch { /* fall through to the source */ }
  const index = readIndex();
  return (shellBridgeCache = { exports: shellExports(index), members: shellMembers(index) });
};

/**
 * What an app's renderer calls on the shells it makes with createShell(): the commands of each member it reaches (`shell.window.close()`),
 * and where it uses a shell in a way that cannot be followed (passed on, returned, or a member the bridge map does not know).
 */
function shellUsage(root, app, members) {
  const out = { commands: new Map(), unresolved: null, bound: false };
  const names = new Set();
  const chainOf = (skel, at) => /^\s*\??\.\s*([A-Za-z_$][\w$]*)(?:\s*\??\.\s*([A-Za-z_$][\w$]*))?/.exec(skel.slice(at, at + 160));
  const use = (f, offset, chain) => {
    const where = { file: f.file, line: lineOf(f.text, offset) };
    const segments = chain ? [chain[1], chain[2]].filter(Boolean) : [];
    for (let k = segments.length; k > 0; k--) {
      const commands = members.get(segments.slice(0, k).join("."));
      if (commands) { for (const c of commands) if (!out.commands.has(c)) out.commands.set(c, where); return; }
    }
    out.unresolved ??= where;
  };
  for (const f of readRenderer(root, app, /\bcreateShell\b/)) {
    for (const m of f.skel.matchAll(/\bcreateShell\s*\(/g)) {
      const open = m.index + m[0].length - 1;
      const close = matchClose(f.skel, open);
      const chain = close < 0 ? null : chainOf(f.skel, close + 1);
      if (chain) { use(f, m.index, chain); continue; }
      const binding = /\b(?:const|let|var)\s+([A-Za-z_$][\w$]*)\s*(?::[^=\n]+?)?=\s*(?:await\s+)?$/.exec(f.skel.slice(Math.max(0, m.index - 120), m.index));
      if (binding) { names.add(binding[1]); out.bound = true; } else out.unresolved ??= { file: f.file, line: lineOf(f.text, m.index) };
    }
  }
  for (const name of names) {
    const word = new RegExp(`(?<![.\\w$])${escapeRegExp(name)}(?![\\w$])`, "g");
    for (const f of readRenderer(root, app, new RegExp(word.source))) {
      // Import and re-export lists name a shell without using it.
      const skipped = [...f.code.matchAll(/\bimport\b[^;'"`]*?\bfrom\s*(['"])[^'"\n]*\1|\bexport\s*\{[^}]*\}/g)].map((x) => [x.index, x.index + x[0].length]);
      for (const m of f.skel.matchAll(word)) {
        if (skipped.some(([s, e]) => m.index >= s && m.index < e)) continue;
        const before = f.skel.slice(Math.max(0, m.index - 24), m.index);
        const after = f.skel.slice(m.index + m[0].length, m.index + m[0].length + 4);
        if (/\b(?:const|let|var|typeof)\s+$/.test(before) || (/[{,]\s*$/.test(before) && /^\s*:(?!:)/.test(after))) continue;
        use(f, m.index, chainOf(f.skel, m.index + m[0].length));
      }
    }
  }
  return out;
}

const shellHandler = perApp(({ app, root, shellIndex }) => {
  const bridge = shellIndex === undefined ? defaultShellBridge() : { exports: shellExports(shellIndex), members: shellMembers(shellIndex) };
  const out = [];
  const renderer = readRenderer(root, app, /rk_shell_|@rightkit\/tauri-shell/);
  const needed = new Map();
  let needsAny = null;
  for (const f of renderer) {
    for (const m of f.code.matchAll(/(['"`])(rk_shell_[a-z0-9_]+)\1/g)) if (!needed.has(m[2])) needed.set(m[2], { file: f.file, line: lineOf(f.text, m.index) });
    for (const m of f.code.matchAll(/\bimport\s+(type\s+)?([^'"`;]*?)\s*from\s*(['"])@rightkit\/tauri-shell\3/g)) {
      if (m[1]) continue;
      const where = { file: f.file, line: lineOf(f.text, m.index) };
      const names = /\{([^}]*)\}/.exec(m[2])?.[1].split(",").map((part) => part.trim()).filter((part) => part && !/^type\s/.test(part)).map((part) => part.split(/\s+as\s+/)[0].trim()) ?? [];
      // A default or namespace import (`import shell from`, `import * as shell from`) reaches every command.
      if (m[2].replace(/\{[^}]*\}/, "").replace(/,/g, "").trim()) needsAny ??= where;
      for (const name of names) {
        const commands = bridge.exports.get(name);
        if (name === "createShell" && bridge.members.size > 0) {
          // The shell client wraps every command; the commands this app needs are the members it reaches.
          const usage = shellUsage(root, app, bridge.members);
          for (const [c, at] of usage.commands) if (!needed.has(c)) needed.set(c, at);
          if (usage.unresolved || (!usage.bound && usage.commands.size === 0)) needsAny ??= usage.unresolved ?? where;
        } else if (commands === undefined || commands.length > FACADE_COMMANDS) needsAny ??= where;
        else for (const c of commands) if (!needed.has(c)) needed.set(c, where);
      }
    }
  }
  if (needed.size === 0 && needsAny === null) return out;
  const handler = handlerInfo(readRust(root, app, /handler\s*!/));
  if (handler.macro) return out;
  const site = handler.sites[0] ?? { file: app.rustFiles.find((f) => /(^|\/)(lib|main)\.rs$/.test(f)) ?? join(app.confDir, "src/lib.rs"), line: 1 };
  for (const [command, where] of needed) {
    if (handler.listed.has(command)) continue;
    out.push({
      file: site.file,
      subject: `missing:${command}`,
      problem: `${site.file}:${site.line} does not register ${command}, which the renderer invokes (${where.file}:${where.line}): the call is refused at run time`,
      fix: `use rightkit_tauri_shell::handler![..] in the invoke handler, or add rightkit_tauri_shell::commands::${command} to tauri::generate_handler![..]`,
    });
  }
  if (needsAny && needed.size === 0 && handler.listed.size === 0) {
    out.push({
      file: site.file,
      subject: "missing:any",
      problem: `${site.file}:${site.line} registers no rk_shell_* command, but the renderer imports @rightkit/tauri-shell (${needsAny.file}:${needsAny.line}), whose calls are rk_shell_* commands`,
      fix: "use rightkit_tauri_shell::handler![..] in the invoke handler, or list the shell commands the renderer calls in tauri::generate_handler![..]",
    });
  }
  return out;
});

// ---- R5 shell-grants ---------------------------------------------------------------------------------------------------------
const shellGrants = perApp(({ app, root }) => {
  const out = [];
  const handler = handlerInfo(readRust(root, app, /handler\s*!|rk_shell_window/));
  if (!registers(handler, "rk_shell_window")) return out;
  const sources = app.capabilityFiles.map((file) => ({ file, text: readText(root, file) })).map((c) => ({ ...c, ...(c.text === null ? { error: "" } : parseJson(c.text)) }));
  const inline = [app.conf, ...app.platforms].filter((c) => !c.error).map((c) => ({ file: c.file, text: c.text, json: { capabilities: getPath(c.json, ["app", "security", "capabilities"]) } }));
  for (const source of sources) {
    if (source.error !== undefined) continue;
    const list = Array.isArray(source.json) ? source.json : [source.json];
    for (const cap of list) for (const id of permissionIds(cap)) out.push(grantFinding(source, id));
  }
  for (const source of inline) for (const cap of Array.isArray(source.json.capabilities) ? source.json.capabilities : []) for (const id of permissionIds(cap)) out.push(grantFinding(source, id));
  return out;
});
// The window grants rk_shell_window mirrors (crates/rightkit-tauri-shell/src/commands.rs: minimize, maximize, unmaximize, toggle-maximize,
// fullscreen, hide, show, close). A grant with no shell verb (set-size, center, set-focus, ...) has no sanctioned replacement, and
// allow-start-dragging stays: Tauri's own `data-tauri-drag-region` handler (the shell title bar) calls plugin:window|start_dragging.
const SHELL_WINDOW_GRANTS = new Set(["minimize", "maximize", "unmaximize", "toggle-maximize", "set-fullscreen", "close", "hide", "show"].map((verb) => `core:window:allow-${verb}`));
const permissionIds = (cap) => (isObject(cap) && Array.isArray(cap.permissions) ? cap.permissions : []).map((p) => (typeof p === "string" ? p : isObject(p) ? p.identifier : undefined)).filter((id) => typeof id === "string" && SHELL_WINDOW_GRANTS.has(id));
const grantFinding = (source, id) => ({ file: source.file,
  subject: id,
  problem: `${source.file}:${lineOfPattern(source.text, new RegExp(`"${escapeRegExp(id)}"`))} grants ${id} while the shell's rk_shell_window command is registered: the shell's window command already does this and runs the shell's caller checks, which a webview-level grant bypasses`,
  fix: `remove "${id}" from this capability and call the shell's window command instead (shell.window.* of @rightkit/tauri-shell)`,
});

// ---- R6 qa-window-label ------------------------------------------------------------------------------------------------------
const qaWindowLabel = perApp(({ app, root }) => {
  const out = [];
  const parsed = readRust(root, app);
  const shellSites = parsed.filter((f) => SHELL_CONSTRUCT.test(f.skel));
  if (shellSites.length === 0) return out;
  const consts = stringConsts(parsed);
  // S: the shell's main window label.
  const labels = [];
  let mainSite = null;
  for (const f of parsed) for (const m of f.skel.matchAll(/\.\s*main_label\s*\(/g)) {
    const [arg] = callArgs(f.skel, m.index + m[0].length - 1);
    const label = arg ? labelOf(f.code.slice(arg[0], arg[1]), consts) : undefined;
    labels.push(label);
    mainSite ??= { file: f.file, line: lineOf(f.text, m.index), label };
  }
  // A label that is not a literal or a string const, or two different ones, cannot be compared: nothing is guessed.
  if (labels.some((l) => l === undefined) || new Set(labels).size > 1) return out;
  const shellLabel = labels[0] ?? "main";
  const shellAt = mainSite ?? { file: shellSites[0].file, line: lineOf(shellSites[0].text, SHELL_CONSTRUCT.exec(shellSites[0].skel).index) };
  // Control registrations and the label each binds.
  for (const f of parsed.filter((p) => /\brightkit_qa\b/.test(p.skel))) {
    const fns = rustFns(f.skel);
    for (const m of f.skel.matchAll(/\bControl\s*(?:::\s*<[^>]*>\s*)?::\s*(?:new|default)\s*\(\s*\)/g)) {
      const fn = innermostFn(fns, m.index);
      // The statement around the registration: back to the previous `;` or block start, forward to its `;` or block.
      let start = m.index;
      for (let depth = 0; start > 0; start--) {
        const c = f.skel[start - 1];
        if (c === ")" || c === "]" || c === "}") depth += 1;
        else if (c === "(" || c === "[") { if (depth > 0) depth -= 1; }
        else if (c === "{") { if (depth === 0) break; depth -= 1; }
        else if (c === ";" && depth === 0) break;
      }
      let end = f.skel.length;
      for (let depth = 0, k = start; k < f.skel.length; k++) {
        const c = f.skel[k];
        if (c === "(" || c === "[") depth += 1;
        else if (c === ")" || c === "]") depth = Math.max(0, depth - 1);
        else if (depth === 0 && (c === ";" || c === "{")) { end = k; break; }
      }
      const statement = f.code.slice(start, end);
      let label = /\.\s*window\s*\(\s*"([^"]*)"\s*\)/.exec(f.code.slice(m.index, end))?.[1];
      const bound = /^\s*let\s+(?:mut\s+)?([A-Za-z_]\w*)\b/.exec(statement)?.[1];
      if (label === undefined && bound && fn) label = new RegExp(`\\b${bound}\\s*\\.\\s*window\\s*\\(\\s*"([^"]*)"\\s*\\)`).exec(f.code.slice(m.index, fn.bodyEnd))?.[1];
      const mismatch = label === undefined ? shellLabel !== "main" : label !== shellLabel;
      if (!mismatch) continue;
      const bind = label === undefined ? 'no .window(..), so it binds "main"' : `.window("${label}")`;
      out.push({
        file: f.file,
        subject: "control-label",
        problem: `${f.file}:${lineOf(f.text, m.index)} registers the QA Control with ${bind}, but ${shellAt.file}:${shellAt.line} sets the shell's main window to "${shellLabel}": every QA order that names the main window answers window_not_found`,
        fix: `add .window("${shellLabel}") to the Control registration at ${f.file}:${lineOf(f.text, m.index)}`,
      });
    }
  }
  // The main label must name a window the app creates.
  const created = new Set();
  let unknownBuilder = false;
  for (const target of app.targets()) for (const w of Array.isArray(getPath(target.merged, ["app", "windows"])) ? getPath(target.merged, ["app", "windows"]) : []) if (isObject(w) && w.create !== false) created.add(typeof w.label === "string" ? w.label : "main");
  for (const f of parsed) for (const m of f.skel.matchAll(/\b(?:WebviewWindowBuilder|WindowBuilder)\s*::\s*new\s*\(|\bwindows\s*::\s*builder\s*\(/g)) {
    const arg = callArgs(f.skel, m.index + m[0].length - 1)[1];
    const label = arg ? labelOf(f.code.slice(arg[0], arg[1]), consts) : undefined;
    if (label === undefined) unknownBuilder = true; else created.add(label);
  }
  if (created.size > 0 && !created.has(shellLabel) && !unknownBuilder) {
    out.push({
      file: shellAt.file,
      subject: "main-label-not-created",
      problem: `${shellAt.file}:${shellAt.line} makes "${shellLabel}" the shell's main window, but no window with that label is created (windows labelled ${[...created].map((l) => `"${l}"`).join(", ")}): show-on-ready, the close policy and QA find nothing`,
      fix: `create a window labelled "${shellLabel}" (tauri.conf.json app.windows or rightkit_tauri_shell::windows::builder), or point main_label at one of the created labels`,
    });
  }
  return out;
});

// ---- R7 devtools-declaration -------------------------------------------------------------------------------------------------
const stripTomlComments = (text) => text.replace(/^\s*#.*$/gm, "").replace(/(\s)#[^"'\n]*$/gm, "$1");
const quotedStrings = (text) => [...text.matchAll(/"([^"]*)"|'([^']*)'/g)].map((m) => m[1] ?? m[2]);
function bracketText(text, open, o, c) { let depth = 0; for (let i = open; i < text.length; i++) { if (text[i] === o) depth += 1; else if (text[i] === c && --depth === 0) return text.slice(open + 1, i); } return text.slice(open + 1); }

/**
 * The shell crate whose `content-devtools` feature a Cargo manifest selects, directly in the dependency or through an app feature
 * (`content-devtools = ["rightkit-tauri-shell/content-devtools"]`), or null. The same test as selectedContentDevtoolsCrate in devtools-policy.mjs.
 */
export function contentDevtoolsCrate(manifestText) {
  const text = stripTomlComments(String(manifestText ?? ""));
  const sections = [];
  const HEADER = /^\s*\[([^\]\n]+)\]\s*$/gm;
  const marks = [...text.matchAll(HEADER)];
  marks.forEach((m, i) => sections.push({ name: m[1].trim(), body: text.slice(m.index + m[0].length, marks[i + 1]?.index ?? text.length) }));
  const features = new Map();
  const dependencies = new Map();
  const info = (spec) => { const list = /features\s*=\s*\[/.exec(spec); return { features: list ? quotedStrings(bracketText(spec, list.index + list[0].length - 1, "[", "]")) : [] }; };
  for (const { name, body } of sections) {
    if (name === "features") {
      for (const m of body.matchAll(/^\s*([A-Za-z0-9_\-"']+)\s*=\s*\[/gm)) features.set(m[1].replace(/^["']|["']$/g, ""), quotedStrings(bracketText(body, m.index + m[0].length - 1, "[", "]")));
    } else if (/^(?:target\.[^\n]+\.)?dependencies$/.test(name)) {
      for (const m of body.matchAll(/^\s*([A-Za-z0-9_\-"']+)\s*=\s*(\{)?/gm)) dependencies.set(m[1].replace(/^["']|["']$/g, ""), info(m[2] ? bracketText(body, m.index + m[0].length - 1, "{", "}") : ""));
    } else {
      const table = /^(?:target\.[^\n]+\.)?dependencies\.([A-Za-z0-9_\-]+)$/.exec(name);
      if (table) dependencies.set(table[1], info(body));
    }
  }
  for (const crate of SHELL_CRATES) {
    const shell = dependencies.get(crate);
    if (!shell) continue;
    if (shell.features.includes("content-devtools")) return crate;
    for (const items of features.values()) if (items.some((item) => item === `${crate}/content-devtools` || item === `${crate}?/content-devtools`)) return crate;
  }
  return null;
}

function readAllowList() {
  try { const parsed = JSON.parse(fs.readFileSync(POLICY_FILE, "utf8")); return Array.isArray(parsed.contentOnlyApps) ? parsed.contentOnlyApps.filter((a) => typeof a === "string") : []; } catch { return []; }
}

const devtoolsDeclaration = perApp(({ app, root, contentOnlyApps }) => {
  const allowed = contentOnlyApps ?? readAllowList();
  const out = [];
  const release = app.releaseConfig;
  const releaseText = release ? readText(root, release) : null;
  let declared = false;
  let appId;
  if (releaseText !== null) {
    if (release.endsWith(".json")) { const cfg = parseJson(releaseText).json; declared = cfg?.devtools === "content-only"; appId = typeof cfg?.app === "string" ? cfg.app : undefined; }
    else { const code = parsedJs(releaseText).code; declared = /\bdevtools\s*:\s*(["'`])content-only\1/.test(code); appId = /(?:^|[\s,{])["']?app["']?\s*:\s*(["'`])([^"'`]+)\1/.exec(code)?.[2]; }
  }
  const cargoText = app.cargoToml ? readText(root, app.cargoToml) : null;
  const feature = cargoText !== null && contentDevtoolsCrate(cargoText) !== null;
  const parsed = readRust(root, app);
  const consts = new Map();
  for (const { code } of parsed) for (const m of code.matchAll(/\b(?:const|static)\s+([A-Z][A-Z0-9_]*)\s*:[^=]*=\s*([^;]*);/g)) consts.set(m[1], m[2]);
  let runtime = false;
  for (const f of parsed) for (const m of f.skel.matchAll(/\.\s*devtools_policy\s*\(/g)) {
    const [arg] = callArgs(f.skel, m.index + m[0].length - 1);
    const argCode = arg ? f.code.slice(arg[0], arg[1]) : "";
    if (/\bContentOnly\b/.test(argCode) || (/^[A-Z][A-Z0-9_]*$/.test(argCode) && /\bContentOnly\b/.test(consts.get(argCode) ?? ""))) runtime = true;
  }
  if (!declared && !feature && !runtime) return out;
  const buildRs = app.buildRs ? readText(root, app.buildRs) : null;
  const buildCode = buildRs === null ? "" : parsedRust(buildRs).code;
  const acl = /\bAppManifest\b/.test(buildCode) && /\.commands\s*\(/.test(buildCode);
  const shellFile = parsed.find((f) => SHELL_CONSTRUCT.test(f.skel))?.file ?? app.rustFiles.find((f) => /(^|\/)(lib|main)\.rs$/.test(f)) ?? join(app.confDir, "src/lib.rs");
  const have = [declared && "the release config declares devtools: \"content-only\"", feature && "Cargo.toml selects content-devtools", runtime && "the Rust calls .devtools_policy(ContentOnly)"].filter(Boolean).join(", ");
  const add = (subject, file, why, fix) => out.push({ file, subject, problem: `${file}:1 ${why} (${have})`, fix });
  const releaseFile = release ?? join(app.appRoot, "right-release.config.json");
  if (!declared) add("release-config", releaseFile, `${release ? "the release config does not declare" : "there is no release config declaring"} devtools: "content-only", and the pre-build DevTools gate refuses a build without it`, `add devtools: "content-only" to ${releaseFile}`);
  if (appId === undefined && releaseText !== null) add("allow-list", release, "the release config names no app, so it cannot be matched against the content-only allow-list", `add app: "<id>" to ${release} and list that id in rightkit's ${ALLOW_LIST_FILE} contentOnlyApps`);
  else if (appId !== undefined && !allowed.includes(appId)) add("allow-list", ALLOW_LIST_FILE, `app "${appId}" is not in rightkit's ${ALLOW_LIST_FILE} contentOnlyApps (${allowed.join(", ") || "empty"}): only an app on that list may declare content-only DevTools`, `list "${appId}" in ${ALLOW_LIST_FILE} contentOnlyApps on the owner's instruction, or drop the declaration, the cargo feature and the runtime policy`);
  if (!feature) add("cargo-feature", app.cargoToml ?? join(app.confDir, "Cargo.toml"), "Cargo.toml does not enable the rightkit-tauri-shell content-devtools feature, so DevToolsPolicy::ContentOnly cannot be selected", 'add "content-devtools" to the rightkit-tauri-shell dependency features');
  if (!acl) add("acl-manifest", app.buildRs ?? join(app.confDir, "build.rs"), "build.rs declares no application ACL manifest (tauri_build::AppManifest with .commands(..)): without one, local content can call every app command", "declare tauri_build::AppManifest::new().commands(..) in build.rs and grant each command in a capability");
  if (!runtime) add("runtime-policy", shellFile, "the Rust never calls .devtools_policy(DevToolsPolicy::ContentOnly) on the shell builder, so the webview keeps the default DevTools denial", "call .devtools_policy(DevToolsPolicy::ContentOnly) on Shell::builder(..)");
  return out;
});

// ---- R8 shell-lifecycle-external ---------------------------------------------------------------------------------------------
const shellLifecycleExternal = perApp(({ app, root }) => {
  const out = [];
  const parsed = readRust(root, app, /lifecycle_only|lifecycle_plugin|WebviewUrl/);
  const external = parsed.find((f) => /\bWebviewUrl\s*::\s*External\b/.test(f.skel));
  if (!external) return out;
  const at = /\bWebviewUrl\s*::\s*External\b/.exec(external.skel);
  for (const f of parsed) {
    const call = /(?<!\bfn\s+)\blifecycle_(?:only|plugin)\s*\(/.exec(f.skel);
    if (!call) continue;
    out.push({
      file: f.file,
      subject: "lifecycle-only+external",
      problem: `${f.file}:${lineOf(f.text, call.index)} installs the shell with ${call[0].replace(/\s*\($/, "")}() while the app opens an external-URL window (${external.file}:${lineOf(external.text, at.index)}): lifecycle_only drops the JS hardening and the navigation policy, so that window navigates unfiltered`,
      fix: "use ShellPlugin::builder().shell(..).host_windows(): it keeps hardening and the navigation policy while the app owns its windows",
    });
  }
  return out;
});

// ---- R9 legal-receipt-native -------------------------------------------------------------------------------------------------
const NATIVE_PORT = /\bcreateNativeLegalReceiptPort\s*\(/;
const JS_KEYWORDS = new Set(["const", "let", "var", "function", "return", "async", "await", "new", "typeof", "void", "null", "undefined", "true", "false", "this", "if", "else", "import", "export", "from", "as", "type", "interface", "default"]);

/** The local name `name` is imported under from `pkg` in `code` (a type-only import does not count), or null. */
function importedAs(code, pkg, name) {
  for (const m of code.matchAll(new RegExp(`\\bimport\\s*\\{([^}]*)\\}\\s*from\\s*(['"])${escapeRegExp(pkg)}\\2`, "g"))) {
    for (const part of m[1].split(",").map((x) => x.trim())) {
      const [from, to] = part.split(/\s+as\s+/);
      if (from === name) return to ?? from;
    }
  }
  return null;
}

/** Bindings of the renderer: Map name -> [{ file, line, text }] (a const/let/var initialiser, or a function with its body). */
function definitionIndex(renderer) {
  const index = new Map();
  const add = (name, f, at, text) => { if (!index.has(name)) index.set(name, []); index.get(name).push({ file: f.file, line: lineOf(f.text, at), text }); };
  for (const f of renderer) {
    for (const m of f.skel.matchAll(/\b(?:async\s+)?function\s*\*?\s+([A-Za-z_$][\w$]*)\s*[(<]/g)) {
      const params = f.skel.indexOf("(", m.index);
      const open = params < 0 ? -1 : f.skel.indexOf("{", matchClose(f.skel, params));
      const close = open < 0 ? -1 : matchClose(f.skel, open);
      add(m[1], f, m.index, f.code.slice(m.index, close < 0 ? m.index + 400 : close + 1));
    }
    for (const m of f.skel.matchAll(/\b(?:const|let|var)\s+([A-Za-z_$][\w$]*)\s*(?::[^=\n]+?)?=(?!=)/g)) add(m[1], f, m.index, f.code.slice(m.index + m[0].length, statementEnd(f.skel, m.index + m[0].length)));
  }
  return index;
}

/** Whether `text` is createNativeLegalReceiptPort(..) or built from a binding that is; `walked` collects the bindings it followed. */
function nativeExpression(text, index, walked = [], seen = new Set(), depth = 0) {
  if (NATIVE_PORT.test(text)) return true;
  if (depth >= 4) return false;
  for (const id of new Set(text.match(/[A-Za-z_$][\w$]*/g) ?? [])) {
    if (JS_KEYWORDS.has(id) || seen.has(id) || !index.has(id)) continue;
    seen.add(id);
    for (const def of index.get(id)) { walked.push({ id, ...def }); if (nativeExpression(def.text, index, walked, seen, depth + 1)) return true; }
  }
  return false;
}

const legalReceiptNative = perApp(({ app, root }) => {
  const out = [];
  const renderer = readRenderer(root, app, /@rightkit\/(?:onboarding|legal-ui)|createNativeLegalReceiptPort/);
  const anyNative = renderer.some((f) => NATIVE_PORT.test(f.code));
  let index = null;
  for (const f of renderer.filter((r) => /\.[jt]sx$/.test(r.file))) {
    const onboarding = importedAs(f.code, "@rightkit/onboarding", "Onboarding");
    for (const m of onboarding ? f.skel.matchAll(new RegExp(`<${escapeRegExp(onboarding)}(?=[\\s/>])`, "g")) : []) {
      const tag = readJsxTag(f.skel, f.code, m.index);
      if (!tag || tag.spread) continue;
      // legalMode="none" and "notice" have no acceptance to receipt; any other computed value cannot be judged from the file.
      const mode = tag.attrs.get("legalMode");
      if (mode) {
        const value = mode.text.replace(/^(["'`])(.*)\1$/, "$2");
        if (value === "none" || value === "notice" || (value !== "gate" && mode.kind === "expr")) continue;
      }
      const receipt = tag.attrs.get("legalReceipt");
      const at = `${f.file}:${lineOf(f.text, m.index)}`;
      if (!receipt) {
        out.push({
          file: f.file,
          subject: "settings-copy",
          problem: `${at} <Onboarding> has no legalReceipt: the acceptance is stamped with the webview clock and kept in settings, with no native receipt`,
          fix: 'pass legalReceipt={createNativeLegalReceiptPort({ invoke })} (@rightkit/onboarding) and install the shell\'s legal receipt service, or pass legalMode="none" when the app has no legal gate',
        });
        continue;
      }
      index ??= definitionIndex(readRenderer(root, app));
      const walked = [];
      if (nativeExpression(receipt.text, index, walked)) continue;
      if (/^[A-Za-z_$][\w$.]*$/.test(receipt.text) && !index.has(receipt.text.split(".")[0])) continue; // a name this app does not define: not judgeable
      const port = walked.find((d) => /\bread\b/.test(d.text) && /\b(?:write|create)\b/.test(d.text)) ?? walked[0];
      const where = port ? `${port.file}:${port.line}` : `${f.file}:${lineOf(f.text, receipt.offset)}`;
      out.push({
        file: f.file,
        subject: "hand-written-port",
        problem: `${at} <Onboarding> takes legalReceipt={${receipt.text.replace(/\s+/g, " ").slice(0, 60)}}, a hand-written port (${where}) that is not createNativeLegalReceiptPort: acceptedAt comes from the webview clock and the app stores the receipt itself`,
        fix: `replace the port at ${where} with createNativeLegalReceiptPort({ invoke }) (@rightkit/onboarding); the native service stamps acceptedAt and stores the receipt`,
      });
    }
    const gate = anyNative ? null : importedAs(f.code, "@rightkit/legal-ui", "LegalGate");
    for (const m of gate ? f.skel.matchAll(new RegExp(`<${escapeRegExp(gate)}(?=[\\s/>])`, "g")) : []) {
      const agree = readJsxTag(f.skel, f.code, m.index)?.attrs.get("onAgree");
      if (!agree) continue;
      // The handler's text: inline, or the function it names in this file.
      const name = /^[A-Za-z_$][\w$]*$/.test(agree.text) ? escapeRegExp(agree.text) : null;
      const named = name ? new RegExp(`\\b(?:function\\s+${name}\\b|(?:const|let)\\s+${name}\\b)[\\s\\S]{0,1500}`).exec(f.code)?.[0] ?? "" : "";
      if (!/\bnew\s+Date\s*\(|\bDate\s*\.\s*now\s*\(|toISOString|accepted_?at|acceptedAt/i.test(`${agree.text}\n${named}`)) continue;
      out.push({
        file: f.file,
        subject: "legal-gate-without-native-receipt",
        problem: `${f.file}:${lineOf(f.text, m.index)} <LegalGate> records the acceptance in the app's own store with a webview-clock timestamp, and no createNativeLegalReceiptPort exists anywhere in this app's renderer`,
        fix: "build the receipt with createNativeLegalReceiptPort({ invoke }) (@rightkit/onboarding) and create it from onAgree: the native service stamps acceptedAt and stores it",
      });
    }
  }
  return out;
});

export const SHELL_CONTRACT_RULES = Object.freeze([
  { id: "shell-window-hidden", gap: "SC-R1", run: shellWindowHidden },
  { id: "shell-plugin-order", gap: "SC-R2", run: shellPluginOrder },
  { id: "shell-handler", gap: "SC-R3", run: shellHandler },
  { id: "shell-csp", gap: "SC-R4", run: shellCsp },
  { id: "shell-grants", gap: "SC-R5", run: shellGrants },
  { id: "qa-window-label", gap: "SC-R6", run: qaWindowLabel },
  { id: "devtools-declaration", gap: "SC-R7", run: devtoolsDeclaration },
  { id: "shell-lifecycle-external", gap: "SC-R8", run: shellLifecycleExternal },
  { id: "legal-receipt-native", gap: "SC-R9", run: legalReceiptNative },
]);
