// `security` in the release config: the Content Security Policy and the Tauri capability set the app ships must not drift from the approved
// values (#1136). Read-only: it reads `tauri.conf.json` and the capability files and writes nothing, so the doctor and the build report the
// same rows (`security-policy` in release.mjs).
//
//   security: {
//     tauriConfig: "src-tauri/tauri.conf.json",                   // optional (this is the default)
//     csp: { exact: "default-src 'self'; connect-src 'self' https://api.example.com" }          // the policy equals this, directive by directive
//        | { allow: { "connect-src": ["'self'", "https://api.example.com"], "default-src": ["'self'"] },   // an allow-list: every directive and source
//            require?: { "connect-src": ["https://api.example.com"] } },                      // the app's policy uses must be listed; `require` must be present
//     capabilities: {
//       dir: "src-tauri/capabilities",                           // optional (this is the default)
//       expected: { main: { exact?: ["core:default", "process:allow-restart"], require?: ["process:allow-restart"], forbid?: ["updater:"] } }
//     }                                                          // `forbid` entries are permission identifier prefixes
//   }
//
// Every capability file in `dir` must be named in `expected` (a new, unreviewed capability is drift), and every `expected` capability must exist.
import { existsSync, readdirSync, readFileSync } from "node:fs";
import { createRequire } from "node:module";
import path from "node:path";

const KEYS = ["tauriConfig", "csp", "capabilities"];
const CSP_KEYS = ["exact", "allow", "require"];
const CAPABILITY_RULE_KEYS = ["exact", "require", "forbid"];
const isObject = (value) => value !== null && typeof value === "object" && !Array.isArray(value);
const stringList = (value) => Array.isArray(value) && value.every((item) => typeof item === "string" && item.trim() !== "");

let smolTomlParse;
const parseToml = (text) => (smolTomlParse ??= createRequire(import.meta.url)("smol-toml").parse)(text);

function insideApp(root, relative, label) {
  if (typeof relative !== "string" || !relative.trim() || path.isAbsolute(relative) || /^[A-Za-z]:/.test(relative) || relative.replaceAll("\\", "/").split("/").includes("..")) {
    throw new Error(`${label} must be a relative path inside the app: ${relative}`);
  }
  return path.join(root, relative);
}

/** { directive: [sources] } of a CSP given as a string, or as the object/array form Tauri accepts. */
export function parseCsp(value) {
  const directives = {};
  const add = (name, sources) => {
    const key = String(name).trim().toLowerCase();
    if (!key) return;
    // The first occurrence of a directive wins in a browser; a repeat is ignored, so it is ignored here too.
    if (!(key in directives)) directives[key] = [...new Set(sources)].sort();
  };
  if (typeof value === "string") {
    for (const part of value.split(";")) {
      const [name, ...sources] = part.trim().split(/\s+/).filter(Boolean);
      if (name) add(name, sources);
    }
  } else if (isObject(value)) {
    for (const [name, sources] of Object.entries(value)) add(name, Array.isArray(sources) ? sources.map(String) : String(sources).split(/\s+/).filter(Boolean));
  } else throw new Error("the Content Security Policy must be a string or a directive map");
  return directives;
}

const describe = (directives) => Object.entries(directives).map(([name, sources]) => `${name} ${sources.join(" ")}`.trim()).join("; ");

/** Problems (empty when the policy is as approved) of one app CSP against `security.csp`. */
export function cspProblems(actual, rule) {
  const problems = [];
  if (rule.exact !== undefined) {
    const expected = parseCsp(rule.exact);
    for (const name of new Set([...Object.keys(expected), ...Object.keys(actual)])) {
      const want = expected[name], have = actual[name];
      if (!have) problems.push(`CSP directive ${name} is approved (${want.join(" ")}) but the app's policy has none`);
      else if (!want) problems.push(`CSP directive ${name} (${have.join(" ")}) is not in the approved policy`);
      else if (want.join(" ") !== have.join(" ")) problems.push(`CSP ${name} is ${have.join(" ")}, the approved policy has ${want.join(" ")}`);
    }
  }
  if (rule.allow !== undefined) {
    const allowed = Object.fromEntries(Object.entries(rule.allow).map(([name, sources]) => [name.toLowerCase(), new Set(sources)]));
    for (const [name, sources] of Object.entries(actual)) {
      if (!allowed[name]) { problems.push(`CSP directive ${name} (${sources.join(" ")}) is not on the approved allow-list`); continue; }
      const extra = sources.filter((source) => !allowed[name].has(source));
      if (extra.length) problems.push(`CSP ${name} allows ${extra.join(" ")}, which the approved allow-list does not`);
    }
  }
  for (const [name, sources] of Object.entries(rule.require ?? {})) {
    const have = new Set(actual[name.toLowerCase()] ?? []);
    const missing = sources.filter((source) => !have.has(source));
    if (missing.length) problems.push(`CSP ${name} must allow ${missing.join(" ")} (the app's API hosts)`);
  }
  return problems;
}

/** Permission identifiers of a capability document ("core:default" or { identifier, allow, deny }). */
export function capabilityPermissions(capability) {
  return [...new Set((capability.permissions ?? []).map((entry) => (typeof entry === "string" ? entry : entry?.identifier)).filter((id) => typeof id === "string"))].sort();
}

/** Validate the `security` block's shape. Throws on the first malformed value. */
export function validateSecurityConfig(security) {
  if (!isObject(security)) throw new Error("security must be an object");
  const unknown = Object.keys(security).filter((key) => !KEYS.includes(key));
  if (unknown.length) throw new Error(`security has unknown key(s): ${unknown.join(", ")} (allowed: ${KEYS.join(", ")})`);
  if (security.csp !== undefined) {
    const csp = security.csp;
    if (!isObject(csp)) throw new Error("security.csp must be an object with exact, allow and/or require");
    const bad = Object.keys(csp).filter((key) => !CSP_KEYS.includes(key));
    if (bad.length) throw new Error(`security.csp has unknown key(s): ${bad.join(", ")} (allowed: ${CSP_KEYS.join(", ")})`);
    if (csp.exact === undefined && csp.allow === undefined && csp.require === undefined) throw new Error("security.csp needs exact, allow or require");
    if (csp.exact !== undefined && csp.allow !== undefined) throw new Error("security.csp: give exact or allow, not both");
    if (csp.exact !== undefined) parseCsp(csp.exact);
    for (const key of ["allow", "require"]) {
      if (csp[key] === undefined) continue;
      if (!isObject(csp[key]) || Object.values(csp[key]).some((sources) => !stringList(sources))) throw new Error(`security.csp.${key} must map directive names to lists of sources`);
    }
  }
  if (security.capabilities !== undefined) {
    const capabilities = security.capabilities;
    if (!isObject(capabilities)) throw new Error("security.capabilities must be an object");
    const bad = Object.keys(capabilities).filter((key) => !["dir", "expected"].includes(key));
    if (bad.length) throw new Error(`security.capabilities has unknown key(s): ${bad.join(", ")} (allowed: dir, expected)`);
    if (!isObject(capabilities.expected) || Object.keys(capabilities.expected).length === 0) throw new Error("security.capabilities.expected must name at least one capability");
    for (const [id, rule] of Object.entries(capabilities.expected)) {
      if (!isObject(rule)) throw new Error(`security.capabilities.expected.${id} must be an object`);
      const badRule = Object.keys(rule).filter((key) => !CAPABILITY_RULE_KEYS.includes(key));
      if (badRule.length) throw new Error(`security.capabilities.expected.${id} has unknown key(s): ${badRule.join(", ")} (allowed: ${CAPABILITY_RULE_KEYS.join(", ")})`);
      if (CAPABILITY_RULE_KEYS.every((key) => rule[key] === undefined)) throw new Error(`security.capabilities.expected.${id} needs exact, require or forbid`);
      for (const key of CAPABILITY_RULE_KEYS) if (rule[key] !== undefined && !stringList(rule[key])) throw new Error(`security.capabilities.expected.${id}.${key} must be a list of permission identifiers`);
    }
  }
  if (security.csp === undefined && security.capabilities === undefined) throw new Error("security needs csp and/or capabilities");
  return security;
}

function readCapabilityFiles(dir) {
  const found = new Map();
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    if (!entry.isFile() || !/\.(json|toml)$/i.test(entry.name)) continue;
    const file = path.join(dir, entry.name);
    let document;
    try {
      const text = readFileSync(file, "utf8");
      document = /\.toml$/i.test(entry.name) ? parseToml(text) : JSON.parse(text);
    } catch (error) {
      throw new Error(`capability file ${entry.name} is not valid ${/\.toml$/i.test(entry.name) ? "TOML" : "JSON"}: ${error.message}`);
    }
    // Tauri names a capability by its `identifier`; a file without one is named by its file stem.
    const id = typeof document.identifier === "string" && document.identifier ? document.identifier : path.basename(entry.name, path.extname(entry.name));
    if (found.has(id)) throw new Error(`capability ${id} is defined twice (${found.get(id).file} and ${entry.name})`);
    found.set(id, { file: entry.name, permissions: capabilityPermissions(document) });
  }
  return found;
}

/** Every drift of the app's CSP and capability set from `security`; throws a single Error naming them all, returns the checked summary otherwise. */
export function assertSecurityPolicy(root, security, label = path.basename(root)) {
  validateSecurityConfig(security);
  const problems = [];
  const checked = [];
  const tauriConfigPath = insideApp(root, security.tauriConfig ?? "src-tauri/tauri.conf.json", "security.tauriConfig");
  if (security.csp !== undefined) {
    if (!existsSync(tauriConfigPath)) throw new Error(`${label} security.csp: ${path.relative(root, tauriConfigPath)} does not exist`);
    let tauriConfig;
    try { tauriConfig = JSON.parse(readFileSync(tauriConfigPath, "utf8")); } catch (error) { throw new Error(`${label} ${path.relative(root, tauriConfigPath)} is not valid JSON: ${error.message}`); }
    // Tauri 2 keeps the policy at app.security.csp, Tauri 1 at tauri.security.csp.
    const raw = tauriConfig?.app?.security?.csp ?? tauriConfig?.tauri?.security?.csp;
    if (raw === undefined || raw === null || raw === "") problems.push(`the app has no Content Security Policy in ${path.relative(root, tauriConfigPath)} (app.security.csp); security.csp requires one`);
    else {
      let actual;
      try { actual = parseCsp(raw); } catch (error) { throw new Error(`${label} ${path.relative(root, tauriConfigPath)}: ${error.message}`); }
      problems.push(...cspProblems(actual, security.csp).map((problem) => `${problem} (policy: ${describe(actual)})`));
      checked.push("csp");
    }
  }
  if (security.capabilities !== undefined) {
    const dir = insideApp(root, security.capabilities.dir ?? "src-tauri/capabilities", "security.capabilities.dir");
    if (!existsSync(dir)) throw new Error(`${label} security.capabilities: ${path.relative(root, dir)} does not exist`);
    const present = readCapabilityFiles(dir);
    const expected = security.capabilities.expected;
    for (const id of present.keys()) if (!(id in expected)) problems.push(`capability ${id} (${present.get(id).file}) is not in security.capabilities.expected: a new capability needs review`);
    for (const [id, rule] of Object.entries(expected)) {
      const entry = present.get(id);
      if (!entry) { problems.push(`capability ${id} is expected but ${path.relative(root, dir)} has no such capability`); continue; }
      const have = new Set(entry.permissions);
      if (rule.exact !== undefined) {
        const want = new Set(rule.exact);
        const extra = [...have].filter((permission) => !want.has(permission));
        const missing = [...want].filter((permission) => !have.has(permission));
        if (extra.length) problems.push(`capability ${id} (${entry.file}) grants ${extra.join(", ")}, which is not in the approved set`);
        if (missing.length) problems.push(`capability ${id} (${entry.file}) lacks ${missing.join(", ")} from the approved set`);
      }
      const missingRequired = (rule.require ?? []).filter((permission) => !have.has(permission));
      if (missingRequired.length) problems.push(`capability ${id} (${entry.file}) must grant ${missingRequired.join(", ")}`);
      for (const prefix of rule.forbid ?? []) {
        const hit = [...have].filter((permission) => permission.startsWith(prefix));
        if (hit.length) problems.push(`capability ${id} (${entry.file}) grants ${hit.join(", ")}, forbidden by ${prefix}`);
      }
    }
    checked.push("capabilities");
  }
  if (problems.length) throw new Error(`${label} security policy drifted from the approved values:\n  - ${problems.join("\n  - ")}`);
  return { checked };
}
