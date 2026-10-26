// Consumer contracts: source contracts that broke consumer builds late (minutes into CI, after install or compile), checked from
// the committed files before anything runs. Each rule is { id, gap, run({ root, files, now }) } like the app-check rules and returns
// findings [{ file, subject, problem, fix }]; `problem` starts with "<file>:<line>" where a line is known. Builtins only: no
// node_modules, no network, no cargo, no pnpm, and no repository code is executed (a right-release.config.mjs is read as text).
//
//   release-config-identity  CA-01  a right-release config with no top-level `app` or `version` (build-release.mjs: "release config
//                                   must expose app and version"; Ember run 38858187099)
//   pnpm-node-floor          CA-05  a CI job that installs pnpm on a Node below what the pinned pnpm requires (pnpm 11 needs Node
//                                   22.13: "No such built-in module: node:sqlite"; CutRight run 30718913302)
//   rust-dependency-msrv     CA-05  a Cargo.lock that resolves a crate whose published rust-version is above the pinned
//                                   rust-toolchain (cargo: "rustc 1.90.0 is not supported by the following packages"; Pulse run 38026724922)
//   release-age              CA-06  a pnpm-lock.yaml entry younger than the workspace's minimumReleaseAge at the check's clock
//                                   (ERR_PNPM_MINIMUM_RELEASE_AGE_VIOLATION; Pulse run 37683011855)
//   notice-lock-digest       CA-14  a third-party notice whose recorded lockfile sha256 is not the committed lockfile's (HeardRight)
//   doc-comment-bare-cr      CA-15  a carriage return not followed by a line feed inside a Rust doc comment (rustc: "bare CR not
//                                   allowed in doc-comment"; Pulse run 38110627257)
//
// Pinned inputs. The pnpm Node floor and default release age, the crates.io rust_version of RightKit's crates and the npm publish
// times of RightKit's packages come from consumer-contracts.json, a snapshot with its source and fetch time; a rule never asks a
// registry. A crate or package version the snapshot does not list is not judged (the snapshot's limit, not a pass). The clock is
// the check's `now` (runRules passes it; tests pin it).
import { createHash } from "node:crypto";
import fs from "node:fs";
import { createRequire } from "node:module";
import path from "node:path";

const DATA_FILE = new URL("./consumer-contracts.json", import.meta.url);
let cachedData = null;
/** The pinned registry and tool metadata the rules read. */
export function contractData() {
  cachedData ??= JSON.parse(fs.readFileSync(DATA_FILE, "utf8"));
  return cachedData;
}

const SKIPPED_TREE = /(^|\/)(node_modules|vendor|target)\//;
// Recorded history is not a live notice: evidence, fixtures, plans and archives keep the digests of their day.
const HISTORY_TREE = /(^|\/)(evidence|release-evidence|receipts?|fixtures?|archive|audits|plans|handoffs?|reports?|provenance)\//i;
const readText = (root, rel) => { try { return fs.readFileSync(path.join(root, rel), "utf8").replace(/^﻿/, ""); } catch { return null; } };
const readBytes = (root, rel) => { try { return fs.readFileSync(path.join(root, rel)); } catch { return null; } };
const lineOf = (text, offset) => { let line = 1; for (let i = text.indexOf("\n"); i !== -1 && i < offset; i = text.indexOf("\n", i + 1)) line += 1; return line; };
const dirOf = (file) => (file.includes("/") ? file.slice(0, file.lastIndexOf("/")) : "");
const join = (dir, rel) => (dir ? `${dir}/${rel}` : rel);
/** `dir` and each directory above it, nearest first, ending with the root (""). */
const upward = (dir) => { const out = []; for (let d = dir; ; d = dirOf(d)) { out.push(d); if (d === "") return out; } };

// ---- versions ---------------------------------------------------------------------------------------------------------------------

/** "1.90", "v22.13.0", "20.x" -> [major, minor|null, patch|null]; null when it is not a plain version (lts/*, latest, an expression). */
export function versionParts(text) {
  const m = /^v?(\d+)(?:\.(\d+|x|\*))?(?:\.(\d+|x|\*))?$/i.exec(String(text).trim());
  if (!m) return null;
  const n = (s) => (s === undefined || /^[x*]$/i.test(s) ? null : Number(s));
  return [Number(m[1]), n(m[2]), n(m[3])];
}
/** Compare two full versions a.b.c; -1, 0 or 1. */
export function compareVersions(a, b) {
  const pa = versionParts(a) ?? [0, 0, 0], pb = versionParts(b) ?? [0, 0, 0];
  for (let i = 0; i < 3; i += 1) { const d = (pa[i] ?? 0) - (pb[i] ?? 0); if (d) return Math.sign(d); }
  return 0;
}
/**
 * Whether a selected version (possibly partial: "20", "22.x") can be below `floor`. A missing part resolves to the newest release of
 * that line (actions/setup-node and rustup both do), so "22" satisfies 22.13.0 and "20" does not.
 */
export function selectionBelow(selected, floor) {
  const s = versionParts(selected), f = versionParts(floor);
  if (!s || !f) return false;
  for (let i = 0; i < 3; i += 1) {
    if (s[i] === null) return false;
    if (s[i] !== (f[i] ?? 0)) return s[i] < (f[i] ?? 0);
  }
  return false;
}
/** The row of `table` ([{ pnpm, ... }], ascending) that applies to pnpm `version`, or null. */
const pnpmRow = (table, version) => [...table].reverse().find((row) => compareVersions(version, row.pnpm) >= 0) ?? null;

/** The pnpm version a package.json pins in packageManager ("pnpm@11.16.0+sha512..." -> "11.16.0"), or null. */
function pnpmPin(root, file) {
  try { return /^pnpm@(\d+\.\d+\.\d+)/.exec(JSON.parse(readText(root, file) ?? "null")?.packageManager ?? "")?.[1] ?? null; } catch { return null; }
}

// ---- CA-01 release-config-identity ------------------------------------------------------------------------------------------------

const CONFIG_FILE = /(^|\/)right-release\.config\.(?:mjs|json)$/;
const SAFE_APP = /^[A-Za-z0-9][A-Za-z0-9._-]*$/;
const SAFE_VERSION = /^[A-Za-z0-9][A-Za-z0-9._+-]*$/;

/** `text` with comments, strings and template literals blanked to spaces (same length, newlines kept), so braces can be counted. */
export function blankJs(text) {
  let out = "";
  for (let i = 0; i < text.length;) {
    const c = text[i], next = text[i + 1];
    const keep = (s) => s.replace(/[^\n]/g, " ");
    if (c === "/" && next === "/") { const end = text.indexOf("\n", i); const stop = end < 0 ? text.length : end; out += keep(text.slice(i, stop)); i = stop; continue; }
    if (c === "/" && next === "*") { const end = text.indexOf("*/", i + 2); const stop = end < 0 ? text.length : end + 2; out += keep(text.slice(i, stop)); i = stop; continue; }
    if (c === "'" || c === '"' || c === "`") {
      let j = i + 1;
      while (j < text.length && text[j] !== c) { if (text[j] === "\\") j += 1; else if (c !== "`" && text[j] === "\n") break; j += 1; }
      out += c + keep(text.slice(i + 1, j)) + (j < text.length ? c : ""); i = j + 1; continue;
    }
    out += c; i += 1;
  }
  return out;
}

/** The top-level keys of the object literal that opens at `open` in blanked `skel`: { keys: Map(name -> offset), spread }. */
function objectKeys(skel, text, open) {
  const keys = new Map();
  let spread = false, depth = 0, expectKey = true;
  for (let i = open; i < skel.length; i += 1) {
    const c = skel[i];
    if (c === "{" || c === "[" || c === "(") { depth += 1; if (depth === 1) expectKey = true; continue; }
    if (c === "}" || c === "]" || c === ")") { depth -= 1; if (depth === 0) return { keys, spread, close: i }; continue; }
    if (depth !== 1) continue;
    if (c === ",") { expectKey = true; continue; }
    if (!expectKey || /\s/.test(c)) continue;
    expectKey = false;
    if (skel.startsWith("...", i)) { spread = true; continue; }
    // A key: an identifier, or a quoted name (blanked in skel; read it from text). Shorthand `version,` counts.
    const quoted = /^(['"])([^'"\n]*)\1\s*:/.exec(text.slice(i));
    if (quoted) { keys.set(quoted[2], i); continue; }
    const ident = /^([A-Za-z_$][\w$]*)\s*(?=[:,}(])/.exec(skel.slice(i));
    if (ident) keys.set(ident[1], i);
  }
  return null;
}

/** The object literal a .mjs config exports by default: { open } or null when it is not a literal this reader can follow. */
function defaultExportObject(skel) {
  const m = /\bexport\s+default\s+/.exec(skel);
  if (!m) return null;
  let at = m.index + m[0].length;
  const call = /^[A-Za-z_$][\w$.]*\s*\(\s*/.exec(skel.slice(at)); // defineConfig({ ... })
  if (call) at += call[0].length;
  if (skel[at] === "{") return { open: at };
  const name = /^([A-Za-z_$][\w$]*)\s*;?/.exec(skel.slice(at))?.[1];
  if (!name) return null;
  const decl = new RegExp(`\\b(?:const|let|var)\\s+${name.replace(/\$/g, "\\$")}\\s*=\\s*`).exec(skel);
  if (!decl) return null;
  const open = decl.index + decl[0].length;
  return skel[open] === "{" ? { open } : null;
}

/** The source text of the value of the property whose key starts at `offset`: { shorthand: name } or { expr, at } (offset of the value). */
function propertyValue(skel, text, offset) {
  const key = /^(['"]?)([A-Za-z_$][\w$-]*)\1\s*/.exec(text.slice(offset));
  if (!key) return null;
  let i = offset + key[0].length;
  if (skel[i] !== ":") return { shorthand: key[2] };
  i += 1;
  let depth = 0, j = i;
  for (; j < skel.length; j += 1) {
    const c = skel[j];
    if (c === "{" || c === "[" || c === "(") depth += 1;
    else if (c === "}" || c === "]" || c === ")") { if (depth === 0) break; depth -= 1; }
    else if (c === "," && depth === 0) break;
  }
  const raw = text.slice(i, j), lead = raw.length - raw.trimStart().length;
  return { expr: raw.trim(), at: i + lead };
}

const NON_STRING = /^(undefined|null|true|false|void\s+0|NaN|-?\d[\d_]*(\.\d+)?([eE][+-]?\d+)?|\[[^]*\]|\{[^]*\})$/;
/**
 * What an expression in the config is, statically: { value } for a string literal (or a top-level `const` bound to one, followed
 * through at most four names), { bad } for an absent or non-string value, { unsupported } for anything this reader cannot prove
 * (a call, a member, an interpolated template, an import, a `let`/`var`, a name with no top-level declaration).
 */
export function staticString(skel, text, expr, seen = []) {
  const str = /^(['"])((?:[^'"\\\n]|\\.)*)\1$/.exec(expr) ?? /^(`)([^`$\\]*)`$/.exec(expr);
  if (str) return { value: str[2] };
  if (NON_STRING.test(expr)) return { bad: `${expr.length > 40 ? `${expr.slice(0, 40)}…` : expr} (not a string)` };
  const name = /^[A-Za-z_$][\w$]*$/.exec(expr)?.[0];
  if (!name) return { unsupported: `the expression \`${expr.length > 60 ? `${expr.slice(0, 60)}…` : expr}\`` };
  if (seen.includes(name) || seen.length >= 4) return { unsupported: `the name chain ${[...seen, name].join(" -> ")}` };
  if (new RegExp(`\\bimport\\b[^;]*\\b${name.replace(/\$/g, "\\$")}\\b[^;]*\\bfrom\\b`).test(skel)) return { unsupported: `\`${name}\`, an imported binding` };
  const decl = new RegExp(`\\b(const|let|var)\\s+${name.replace(/\$/g, "\\$")}\\s*=\\s*`, "g");
  for (let m; (m = decl.exec(skel));) {
    const before = skel.slice(0, m.index);
    const depth = (before.match(/[{([]/g) ?? []).length - (before.match(/[})\]]/g) ?? []).length;
    if (depth !== 0) continue; // only top-level declarations
    if (m[1] !== "const") return { unsupported: `\`${name}\`, declared with ${m[1]} (it can be reassigned)` };
    const at = m.index + m[0].length;
    let d = 0, j = at;
    for (; j < skel.length; j += 1) {
      const c = skel[j];
      if (c === "{" || c === "[" || c === "(") d += 1;
      else if (c === "}" || c === "]" || c === ")") d -= 1;
      else if ((c === ";" || c === "\n") && d === 0) break;
    }
    const resolved = staticString(skel, text, text.slice(at, j).trim(), [...seen, name]);
    return resolved.unsupported && !seen.length ? { unsupported: `\`${name}\` = ${resolved.unsupported}` } : resolved;
  }
  return { unsupported: `\`${name}\`, which has no top-level declaration in this file` };
}

let tomlParse;
/** smol-toml's parse (the TOML parser release-config.mjs uses), loaded synchronously from this package's own dependencies, or null. */
function tomlParser() {
  if (tomlParse === undefined) { try { tomlParse = createRequire(import.meta.url)("smol-toml").parse; } catch { tomlParse = null; } }
  return tomlParse;
}

/**
 * The string a { $fromJson: { file, pointer } } or { $fromToml: { file, path } } node selects, resolved as release-config.mjs does
 * (readInside :173-184, jsonPointerGet :186-196, the $fromToml walk :226-239) over the committed file: { value } for a non-empty string,
 * { bad } for a missing file, pointer or path, or a non-string value, { unsupported } when no TOML parser is installed.
 */
export function resolveTyped(root, set, appDir, node) {
  const json = "$fromJson" in node;
  const spec = json ? node.$fromJson : node.$fromToml;
  const kind = json ? "$fromJson" : "$fromToml";
  if (!spec || typeof spec !== "object" || typeof spec.file !== "string" || (!json && typeof spec.path !== "string")) return { bad: `has a ${kind} that is not { file, ${json ? "pointer" : "path"} }` };
  const rel = spec.file.replaceAll("\\", "/");
  if (!rel.trim() || rel.startsWith("/") || /^[A-Za-z]:/.test(rel) || rel.split("/").includes("..")) return { bad: `reads ${JSON.stringify(spec.file)}, which is not a relative path inside the app` };
  const file = path.posix.normalize(join(appDir, rel));
  if (!set.has(file)) return { bad: `reads ${JSON.stringify(spec.file)}, which is not a committed file` };
  const text = readText(root, file);
  let current;
  const where = json ? (spec.pointer ?? "") : spec.path;
  if (json) {
    try { current = JSON.parse(text); } catch { return { bad: `reads ${spec.file}, which is not valid JSON` }; }
    if (where !== "") {
      if (typeof where !== "string" || !where.startsWith("/")) return { bad: `has a pointer ${JSON.stringify(where)} that is not a JSON pointer such as /version` };
      for (const raw of where.slice(1).split("/")) {
        const k = raw.replaceAll("~1", "/").replaceAll("~0", "~");
        if (!current || typeof current !== "object" || !Object.prototype.hasOwnProperty.call(current, k)) return { bad: `selects ${where}, which is not in ${spec.file}` };
        current = current[k];
      }
    }
  } else {
    const parse = tomlParser();
    if (!parse) return { unsupported: `a $fromToml node, and no TOML parser (smol-toml) is installed beside this package` };
    try { current = parse(text); } catch { return { bad: `reads ${spec.file}, which is not valid TOML` }; }
    for (const k of where.split(".")) {
      if (!current || typeof current !== "object" || Array.isArray(current) || !Object.prototype.hasOwnProperty.call(current, k)) return { bad: `selects ${where}, which is not in ${spec.file}` };
      current = current[k];
    }
  }
  if (typeof current !== "string") return { bad: `selects ${spec.file} ${where || "(the whole file)"}, which is ${current === null ? "null" : Array.isArray(current) ? "an array" : typeof current} (not a string)` };
  if (!current) return { bad: `selects ${spec.file} ${where}, which is an empty string` };
  return { value: current };
}

const UNSUPPORTED_BOUND = "release-config-identity proves a key only from a string literal or a top-level const bound to one in the same file";

function releaseConfigIdentity({ root, files }) {
  const out = [];
  const set = new Set(files);
  for (const file of files.filter((f) => CONFIG_FILE.test(f) && !SKIPPED_TREE.test(f))) {
    const text = readText(root, file);
    if (text === null) continue;
    const add = (key, line, why) => out.push({
      file, subject: key, problem: `${file}:${line} ${why}: right-release build refuses it ("release config must expose app and version", build-release.mjs) after install, before it compiles`,
      fix: `export a top-level ${key} string (a literal, a top-level const string, or in right-release.config.json a $fromJson/$fromToml node)`,
    });
    const unsupported = (key, line, what) => out.push({
      file, subject: `${key}:unsupported`, problem: `${file}:${line} UNSUPPORTED: \`${key}\` is ${what}; ${UNSUPPORTED_BOUND}, so it is not proven present (right-release build refuses a missing one)`,
      fix: `make ${key} a string literal or a top-level const string, or move the config to right-release.config.json with { "$fromToml": { "file": ..., "path": ... } }`,
    });
    if (file.endsWith(".json")) {
      let config;
      try { config = JSON.parse(text); } catch { continue; } // invalid JSON is release-config's (G16) finding
      if (!config || typeof config !== "object" || Array.isArray(config) || !("schema" in config)) continue;
      for (const key of ["app", "version"]) {
        const value = config[key];
        // release-config.mjs expands a { $fromJson } / { $fromToml } node at load (release-config.mjs:217-239); the value it selects
        // is resolved here the same way, from the committed bytes, and must be a non-empty string.
        if (value && typeof value === "object" && !Array.isArray(value) && ("$fromJson" in value || "$fromToml" in value)) {
          const r = resolveTyped(root, set, dirOf(file), value);
          if (r.unsupported) unsupported(key, 1, r.unsupported);
          else if (r.bad) add(key, 1, `"${key}" ${r.bad}`);
          else if (!(key === "app" ? SAFE_APP : SAFE_VERSION).test(r.value)) add(key, 1, `"${key}" resolves to ${JSON.stringify(r.value)}, which is not a safe release-id component`);
          continue;
        }
        if (typeof value === "string" && value.includes("{{")) continue; // a {{token}} expands from the other values at load
        if (typeof value !== "string" || !value) add(key, 1, `has no top-level "${key}" string (found ${JSON.stringify(value ?? null)})`);
        else if (!(key === "app" ? SAFE_APP : SAFE_VERSION).test(value)) add(key, 1, `"${key}": ${JSON.stringify(value)} is not a safe release-id component`);
      }
      continue;
    }
    const skel = blankJs(text);
    const target = defaultExportObject(skel);
    const declaresSchema = /\bschema\s*:/.test(skel);
    if (!target) { if (declaresSchema) unsupported("default", 1, "exported in a form this reader cannot follow (not an object literal, a wrapped one, or a const bound to one)"); continue; }
    const literal = objectKeys(skel, text, target.open);
    if (!literal) continue;
    // Only a config that declares itself a release config (schema) is judged: partial stubs (`export default {}`) are other tools'.
    if (!literal.keys.has("schema")) continue;
    for (const key of ["app", "version"]) {
      if (!literal.keys.has(key)) {
        if (literal.spread) unsupported(key, lineOf(text, target.open), "absent from the literal, which spreads another object that may or may not supply it");
        else add(key, lineOf(text, target.open), `the default export has no top-level \`${key}\``);
        continue;
      }
      const at = literal.keys.get(key);
      const prop = propertyValue(skel, text, at);
      const resolved = !prop ? { unsupported: "an unreadable property" } : staticString(skel, text, prop.shorthand ?? prop.expr);
      if (resolved.unsupported) unsupported(key, lineOf(text, at), resolved.unsupported);
      else if (resolved.bad) add(key, lineOf(text, at), `\`${key}\` is ${resolved.bad}`);
      else if (!resolved.value) add(key, lineOf(text, at), `\`${key}\` is an empty string`);
      else if (!(key === "app" ? SAFE_APP : SAFE_VERSION).test(resolved.value)) add(key, lineOf(text, at), `\`${key}: ${JSON.stringify(resolved.value)}\` is not a safe release-id component`);
    }
  }
  return out;
}

// ---- CA-05 pnpm-node-floor --------------------------------------------------------------------------------------------------------

const WORKFLOW = /^\.github\/workflows\/[^/]+\.ya?ml$/;
const unquote = (s) => s.replace(/\s+#.*$/, "").trim().replace(/^(['"])(.*)\1$/, "$2");

/** The jobs of a workflow as { id, line, steps: [{ line, lines: [{ no, text }] }] }, read by indentation (no YAML library). */
export function workflowJobs(text) {
  const lines = text.split(/\r?\n/).map((t, i) => ({ no: i + 1, text: t }));
  const jobsAt = lines.findIndex((l) => /^jobs:\s*(#.*)?$/.test(l.text));
  if (jobsAt < 0) return [];
  const indent = (t) => /^ */.exec(t)[0].length;
  const body = [];
  for (const l of lines.slice(jobsAt + 1)) { if (/^\S/.test(l.text)) break; body.push(l); }
  const content = body.filter((l) => l.text.trim() && !/^\s*#/.test(l.text));
  if (!content.length) return [];
  const jobIndent = indent(content[0].text);
  const jobs = [];
  for (const l of content) {
    if (indent(l.text) === jobIndent && /^\s*[\w-]+:\s*(#.*)?$/.test(l.text)) jobs.push({ id: l.text.trim().replace(/:.*$/, ""), line: l.no, lines: [] });
    else jobs.at(-1)?.lines.push(l);
  }
  for (const job of jobs) {
    job.steps = [];
    let dash = null;
    for (const l of job.lines) {
      const m = /^(\s*)-\s/.exec(l.text);
      if (m && (dash === null || m[1].length === dash)) { dash = m[1].length; job.steps.push({ line: l.no, lines: [{ no: l.no, text: l.text.slice(dash + 2) }] }); continue; }
      if (dash !== null && indent(l.text) <= dash) { dash = null; continue; } // left the steps list
      job.steps.at(-1)?.lines.push(l);
    }
  }
  return jobs;
}
const stepValue = (step, key) => {
  for (const l of step.lines) { const m = new RegExp(`^\\s*${key}:\\s*(.*)$`).exec(l.text); if (m) return { value: unquote(m[1]), line: l.no }; }
  return null;
};

function pnpmNodeFloor({ root, files }) {
  const data = contractData().pnpm;
  const pins = files.filter((f) => /(^|\/)package\.json$/.test(f) && !SKIPPED_TREE.test(f)).map((f) => ({ file: f, pnpm: pnpmPin(root, f) })).filter((p) => p.pnpm);
  const floors = pins.map((p) => ({ ...p, node: pnpmRow(data.nodeFloor, p.pnpm)?.node })).filter((p) => p.node);
  if (!floors.length) return [];
  const need = floors.reduce((a, b) => (compareVersions(b.node, a.node) > 0 ? b : a));
  const why = `${need.file} pins pnpm ${need.pnpm} (packageManager), which requires Node ${need.node}; on an older Node pnpm stops before installing ("No such built-in module: node:sqlite")`;
  const out = [];
  for (const file of files.filter((f) => WORKFLOW.test(f))) {
    const text = readText(root, file);
    if (text === null) continue;
    for (const job of workflowJobs(text)) {
      const usesPnpm = job.steps.some((s) => /pnpm\/action-setup@/.test(stepValue(s, "uses")?.value ?? "") || /(^|[\s;&|(])pnpm(\s|$)/m.test(s.lines.map((l) => l.text).join("\n")));
      if (!usesPnpm) continue;
      for (const step of job.steps) {
        if (!/^actions\/setup-node@/.test(stepValue(step, "uses")?.value ?? "")) continue;
        let selected = stepValue(step, "node-version");
        const versionFile = stepValue(step, "node-version-file");
        if (!selected && versionFile && files.includes(versionFile.value)) selected = { value: (readText(root, versionFile.value) ?? "").trim().split(/\s+/)[0], line: versionFile.line };
        if (!selected || !selectionBelow(selected.value, need.node)) continue;
        out.push({ file, subject: `${job.id}:node-${selected.value}`, problem: `${file}:${selected.line} job ${job.id} sets up Node ${selected.value} and runs pnpm: ${why}`, fix: `select Node ${need.node} or later in that setup-node step (or pin a pnpm whose engines accept the Node it uses)` });
      }
    }
  }
  for (const file of files.filter((f) => /(^|\/)\.(node-version|nvmrc)$/.test(f) && !SKIPPED_TREE.test(f))) {
    const selected = (readText(root, file) ?? "").trim().split(/\s+/)[0];
    if (selectionBelow(selected, need.node)) out.push({ file, subject: `node-${selected}`, problem: `${file}:1 selects Node ${selected}: ${why}`, fix: `select Node ${need.node} or later` });
  }
  return out;
}

// ---- CA-05 rust-dependency-msrv ---------------------------------------------------------------------------------------------------

const CRATES_IO = /^(registry\+https:\/\/github\.com\/rust-lang\/crates\.io-index|sparse\+https:\/\/index\.crates\.io\/)$/;

/** The [[package]] entries of a Cargo.lock: [{ name, version, source, line }]. */
export function lockPackages(text) {
  const out = [];
  const re = /^\[\[package\]\]\s*$/gm;
  let m;
  while ((m = re.exec(text))) {
    const end = text.indexOf("\n[", m.index + 1);
    const block = text.slice(m.index, end < 0 ? text.length : end);
    const field = (k) => new RegExp(`^${k}\\s*=\\s*"([^"]*)"`, "m").exec(block)?.[1] ?? null;
    out.push({ name: field("name"), version: field("version"), source: field("source"), line: lineOf(text, m.index) });
  }
  return out;
}

/** The toolchain a rust-toolchain(.toml) selects: { channel } or null. */
export function toolchainChannel(text) {
  const toml = /^\s*channel\s*=\s*"([^"]+)"/m.exec(text);
  if (toml) return toml[1].trim();
  const legacy = text.trim();
  return /^[\w.-]+$/.test(legacy) ? legacy : null;
}

function rustDependencyMsrv({ root, files }) {
  const table = contractData().cratesRustVersion.crates;
  const set = new Set(files);
  const out = [];
  for (const lock of files.filter((f) => /(^|\/)Cargo\.lock$/.test(f) && !SKIPPED_TREE.test(f))) {
    const toolchainFile = upward(dirOf(lock)).flatMap((d) => [join(d, "rust-toolchain.toml"), join(d, "rust-toolchain")]).find((f) => set.has(f));
    if (!toolchainFile) continue;
    const channel = toolchainChannel(readText(root, toolchainFile) ?? "");
    if (!channel || !versionParts(channel)) continue; // stable/nightly: not a pinned toolchain, nothing to compare
    const text = readText(root, lock);
    if (text === null) continue;
    for (const pkg of lockPackages(text)) {
      if (!pkg.source || !CRATES_IO.test(pkg.source)) continue;
      const required = table[pkg.name]?.[pkg.version];
      if (!required || !selectionBelow(channel, required)) continue;
      out.push({
        file: lock, subject: `${pkg.name}@${pkg.version}`,
        problem: `${lock}:${pkg.line} resolves ${pkg.name} ${pkg.version}, which requires rustc ${required} (crates.io rust_version), but ${toolchainFile} selects ${channel}: cargo refuses it ("rustc ${channel} is not supported by the following packages")`,
        fix: `raise ${toolchainFile} to ${required} or later, or lock a ${pkg.name} version whose rust-version ${channel} meets`,
      });
    }
  }
  return out;
}

// ---- CA-06 release-age ------------------------------------------------------------------------------------------------------------

/** The top-level scalar and list settings of a pnpm-workspace.yaml that release age reads. */
export function workspaceReleaseAge(text) {
  const lines = (text ?? "").split(/\r?\n/);
  let minutes = null;
  const exclude = [];
  for (let i = 0; i < lines.length; i += 1) {
    const age = /^minimumReleaseAge:\s*(.+)$/.exec(lines[i]);
    if (age) { const n = Number(unquote(age[1])); if (Number.isFinite(n)) minutes = n; continue; }
    const ex = /^minimumReleaseAgeExclude:\s*(.*)$/.exec(lines[i]);
    if (!ex) continue;
    const inline = unquote(ex[1]);
    if (inline.startsWith("[")) { for (const item of inline.replace(/^\[|\]$/g, "").split(",")) if (item.trim()) exclude.push(unquote(item)); continue; }
    if (inline) { exclude.push(inline); continue; }
    for (let j = i + 1; j < lines.length && (/^\s+-\s/.test(lines[j]) || !lines[j].trim() || /^\s+#/.test(lines[j])); j += 1) {
      const item = /^\s+-\s+(.+)$/.exec(lines[j]);
      if (item) exclude.push(unquote(item[1]));
    }
  }
  return { minutes, exclude };
}

/** Whether an exclude entry (exact `name@version`, `name@v1 || v2`, a bare name, or a `*` pattern) covers `name`@`version`. */
export function excluded(entries, name, version) {
  return entries.some((entry) => {
    const at = entry.lastIndexOf("@");
    const pattern = at > 0 ? entry.slice(0, at) : entry;
    const versions = at > 0 ? entry.slice(at + 1).split("||").map((v) => v.trim()) : null;
    const re = new RegExp(`^${pattern.split("*").map((s) => s.replace(/[.+?^${}()|[\]\\]/g, "\\$&")).join(".*")}$`);
    return re.test(name) && (!versions || versions.includes(version));
  });
}

/** The name@version keys under `packages:` of a pnpm-lock.yaml: [{ name, version, line }]. */
export function pnpmLockPackages(text) {
  const out = [];
  const lines = text.split(/\r?\n/);
  const start = lines.findIndex((l) => /^packages:\s*$/.test(l));
  if (start < 0) return out;
  for (let i = start + 1; i < lines.length && !/^\S/.test(lines[i]); i += 1) {
    const m = /^ {2}(['"]?)((?:@[^@/'"\s]+\/)?[^@'"\s]+)@([^('"\s:]+)(?:\([^'"]*\))?\1:\s*$/.exec(lines[i]);
    if (m) out.push({ name: m[2], version: m[3], line: i + 1 });
  }
  return out;
}

function releaseAge({ root, files, now = new Date(), registry = contractData().npmPublishTimes.packages }) {
  const out = [];
  const set = new Set(files);
  for (const lock of files.filter((f) => /(^|\/)pnpm-lock\.yaml$/.test(f) && !SKIPPED_TREE.test(f))) {
    const dir = dirOf(lock);
    const workspaceFile = join(dir, "pnpm-workspace.yaml");
    const policy = workspaceReleaseAge(set.has(workspaceFile) ? readText(root, workspaceFile) : "");
    let minutes = policy.minutes, from = `${workspaceFile} minimumReleaseAge`;
    if (minutes === null) {
      const pinFile = upward(dir).map((d) => join(d, "package.json")).find((f) => set.has(f) && pnpmPin(root, f));
      const row = pinFile ? pnpmRow(contractData().pnpm.defaultMinimumReleaseAge, pnpmPin(root, pinFile)) : null;
      if (!row) continue;
      minutes = row.minutes; from = `the pnpm ${pnpmPin(root, pinFile)} default (${pinFile} packageManager)`;
    }
    if (!(minutes > 0)) continue;
    const cutoff = new Date(now.getTime() - minutes * 60 * 1000);
    const text = readText(root, lock);
    if (text === null) continue;
    for (const pkg of pnpmLockPackages(text)) {
      const published = registry[pkg.name]?.[pkg.version];
      if (!published || new Date(published) <= cutoff || excluded(policy.exclude, pkg.name, pkg.version)) continue;
      out.push({
        file: lock, subject: `${pkg.name}@${pkg.version}`,
        problem: `${lock}:${pkg.line} ${pkg.name}@${pkg.version} was published at ${published}, within the minimumReleaseAge cutoff (${cutoff.toISOString()}; ${minutes} minutes from ${from}): pnpm install --frozen-lockfile refuses it (ERR_PNPM_MINIMUM_RELEASE_AGE_VIOLATION)`,
        fix: `wait until ${new Date(new Date(published).getTime() + minutes * 60 * 1000).toISOString()}, or add the reviewed exact version '${pkg.name}@${pkg.version}' to minimumReleaseAgeExclude in ${workspaceFile}`,
      });
    }
  }
  return out;
}

// ---- CA-14 notice-lock-digest -----------------------------------------------------------------------------------------------------

const NOTICE_FILE = /(^|\/)[^/]*(notice|licen[cs]e|third[-_ ]?party)[^/]*\.(html?|md|txt)$/i;
const LOCK_NAME = String.raw`(?:[\w.-]+\/)*(?:pnpm-lock\.yaml|Cargo\.lock|package-lock\.json|yarn\.lock|bun\.lock)`;
// The two formats the generators write, both read here:
//   `<lock> sha256=<hex>`  packages/release/npm-license-notices.mjs:74 and packages/legal/src/tooling.ts:263 (npm appendix); HeardRight's
//                          own generator quotes it as `pnpm-lock.yaml` sha256=`<hex>`
//   `<lock> sha256 <hex>`  packages/legal/src/tooling.ts:142-149 (legal-snapshot fills {{key}} of a notices template with the sha256 of
//                          notices.hashes[key]); desktop-app-template/legal/NOTICES.template.md:4 `pnpm-lock.yaml sha256 {{lock}}`
export const LOCK_DIGEST = new RegExp(String.raw`` + "`?(" + LOCK_NAME + ")`?" + String.raw`\s+sha256(?:=|[ \t]+)` + "`?" + String.raw`([0-9a-f]{64})\b`, "g");
const LOCK_LINE = new RegExp(`(${LOCK_NAME})`);
const HEX64 = /\b[0-9a-fA-F]{64}\b/g;

/** The lock files a legal-snapshot config hashes into `notice` (packages/legal/src/tooling.ts: notices.hashes, relative to the app root). */
function snapshotHashFiles(root, set, notice) {
  for (const dir of upward(dirOf(notice))) {
    const configFile = join(dir, "legal-snapshot.config.json");
    if (!set.has(configFile)) continue;
    try {
      const config = JSON.parse(readText(root, configFile));
      const output = config?.notices?.path && path.posix.normalize(join(dir, join(config.legalDir ?? "legal", config.notices.path)));
      if (output !== notice) return [];
      return Object.values(config.notices.hashes ?? {}).filter((f) => typeof f === "string").map((f) => path.posix.normalize(join(dir, f)));
    } catch { return []; }
  }
  return [];
}

function noticeLockDigest({ root, files }) {
  const set = new Set(files);
  const out = [];
  const digests = new Map();
  const digest = (file) => {
    if (!digests.has(file)) { const bytes = readBytes(root, file); digests.set(file, bytes && createHash("sha256").update(bytes).digest("hex")); }
    return digests.get(file);
  };
  for (const file of files.filter((f) => NOTICE_FILE.test(f) && !SKIPPED_TREE.test(f) && !HISTORY_TREE.test(f))) {
    const text = readText(root, file);
    if (text === null) continue;
    const declared = snapshotHashFiles(root, set, file);
    const seen = new Set();
    const consumed = new Set();
    for (const m of text.matchAll(LOCK_DIGEST)) {
      const [, ref, recorded] = m;
      consumed.add(m.index + m[0].lastIndexOf(recorded));
      const line = lineOf(text, m.index);
      const lock = declared.find((f) => f === ref || f.endsWith(`/${ref}`)) ?? upward(dirOf(file)).map((d) => join(d, ref)).find((f) => set.has(f));
      if (!lock || !set.has(lock)) {
        out.push({ file, subject: `${ref}:unresolved`, problem: `${file}:${line} UNSUPPORTED: records ${ref} sha256 ${recorded}, but no committed ${ref} at or above ${dirOf(file) || "."} (nor in a legal-snapshot notices.hashes entry) can be compared, so its freshness is not proven`, fix: `commit the lock the notice was generated from beside the app, or regenerate the notice from the lock that is committed` });
        continue;
      }
      if (seen.has(`${lock}|${recorded}`)) continue;
      seen.add(`${lock}|${recorded}`);
      const actual = digest(lock);
      if (actual === recorded) continue;
      out.push({
        file, subject: lock,
        problem: `${file}:${line} records ${ref} sha256 ${recorded}, but the committed ${lock} hashes to ${actual}: the notice was generated from another lockfile`,
        fix: `regenerate the notice from the committed ${lock} (the app's notice generator) and commit both together`,
      });
    }
    // A digest beside a lockfile name in a format neither generator writes is not silently passed.
    const lines = text.split("\n");
    let offset = 0;
    for (let i = 0; i < lines.length; offset += lines[i].length + 1, i += 1) {
      if (!LOCK_LINE.test(lines[i])) continue;
      let previous = 0;
      for (const hex of lines[i].matchAll(HEX64)) {
        const segment = lines[i].slice(previous, hex.index);
        previous = hex.index + hex[0].length;
        if (consumed.has(offset + hex.index)) continue;
        // The digest belongs to the last file named before it: judge it only when that file is a lockfile.
        const owner = [...segment.matchAll(/[\w./-]+\.[A-Za-z]+/g)].at(-1)?.[0] ?? "";
        if (!LOCK_LINE.test(owner)) continue;
        out.push({ file, subject: `${owner}:format:${i + 1}`, problem: `${file}:${i + 1} UNSUPPORTED: a sha256-length digest after ${owner} in a form this rule does not read (it reads the generated \`<lock> sha256=<hex>\` and \`<lock> sha256 <hex>\`), so its freshness is not proven`, fix: "write the digest as `<lock> sha256=<hex>` or `<lock> sha256 <hex>` (what @rightkit/legal legal-snapshot and right-release license-notices write), or regenerate the notice with them" });
        break;
      }
    }
  }
  return out;
}

// ---- CA-15 doc-comment-bare-cr ----------------------------------------------------------------------------------------------------

/** Each bare CR (a \r not followed by \n) inside a Rust doc comment: [{ line, column }]. */
export function docCommentBareCrs(text) {
  const found = [];
  const lines = text.split("\n");
  let block = false; // inside /** or /*! (a doc block); plain /* */ blocks are not doc comments
  for (let i = 0; i < lines.length; i += 1) {
    const line = lines[i].endsWith("\r") ? lines[i].slice(0, -1) : lines[i];
    const lineDoc = /^\s*\/\/(\/(?!\/)|!)/.test(line);
    let doc = block || lineDoc;
    if (!block) { const open = /\/\*(\*(?![*/])|!)/.exec(line); if (open && !lineDoc) { doc = true; block = !line.slice(open.index + 3).includes("*/"); } }
    else if (line.includes("*/")) block = false;
    if (!doc) continue;
    for (let at = line.indexOf("\r"); at !== -1; at = line.indexOf("\r", at + 1)) found.push({ line: i + 1, column: at + 1 });
  }
  return found;
}

function docCommentBareCr({ root, files }) {
  const out = [];
  for (const file of files.filter((f) => f.endsWith(".rs") && !SKIPPED_TREE.test(f))) {
    const bytes = readBytes(root, file);
    if (!bytes || !bytes.includes(13)) continue;
    for (const hit of docCommentBareCrs(bytes.toString("latin1"))) {
      out.push({ file, subject: `line-${hit.line}`, problem: `${file}:${hit.line}:${hit.column} a carriage return that ends no line sits inside a doc comment: rustc refuses the file ("bare CR not allowed in doc-comment")`, fix: "remove the stray \\r byte (a backslash path such as bridge\\relay.json written through a tool that read \\r as an escape); write a literal backslash instead" });
    }
  }
  return out;
}

export const CONSUMER_CONTRACT_RULES = Object.freeze([
  { id: "release-config-identity", gap: "CA-01", run: releaseConfigIdentity },
  { id: "pnpm-node-floor", gap: "CA-05", run: pnpmNodeFloor },
  { id: "rust-dependency-msrv", gap: "CA-05", run: rustDependencyMsrv },
  { id: "release-age", gap: "CA-06", run: releaseAge },
  { id: "notice-lock-digest", gap: "CA-14", run: noticeLockDigest },
  { id: "doc-comment-bare-cr", gap: "CA-15", run: docCommentBareCr },
]);
