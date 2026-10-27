// The release config contract (M24): `right-release.config.json`, a finite, versioned JSON schema read by dev, build, doctor, install
// and upload alike. A `.json` config is data: it is never imported or evaluated. `loadReleaseConfigSync` judges the file NAME before
// anything is opened (an executable name handed to it is refused with RELEASE_CONFIG_LEGACY_EXECUTABLE) and loads the file strictly.
//
// Legacy branch: an app that has not moved yet owns only a `right-release.config.mjs`. `defaultReleaseConfigFile` keeps naming it while
// no `right-release.config.json` sits beside it, and `importReleaseConfig` still imports that module (executable, outside M24; the
// pipeline scanner reports it as R0-legacy-config). The sunset of the legacy branch is tracked separately. As soon as the JSON file
// exists it wins by default and an explicit `--config x.json` always takes the strict loader; the module is never read for it.
//
// Load order (build, dev, upload): resolve the primary checkout and the config path first, then `loadReleaseConfigSync` parses the JSON
// strictly (no comments, no trailing commas, no duplicate keys), checks `schema: 1` and the finite top-level key set, expands the
// typed templates below, and runs the M24 pipeline validation (`pipeline-config.mjs`) so a command that is not an admitted RightKit
// invocation stops the run before the #340 preflight and before any recipe executes. A hash alone is never trusted: each effect
// boundary (build, sign, upload) loads and validates again.
//
// Typed templates (the only computation a config may express):
//   { "$fromJson": { "file": "src-tauri/tauri.conf.json", "pointer": "/version" } }   a string read from an app-relative JSON file
//   { "$fromToml": { "file": "src-tauri/Cargo.toml", "path": "package.version" } }    a string read from an app-relative TOML file
//   { "$json": <value> }                                                              JSON.stringify(<value>), for a JSON argument
//   "{{app}}" "{{version}}" "{{home}}" "{{localAppData}}"                              replaced inside any string value, `$comment`
//                                                                                     included (write no token in a comment). localAppData is
//                                                                                     %LOCALAPPDATA% on Windows and <home>/AppData/Local elsewhere
// Anything else with braces is left alone, and an unknown {{token}} is an error.
import { createHash } from "node:crypto";
import { existsSync, lstatSync, readFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { pathToFileURL } from "node:url";
import { parse as parseToml } from "smol-toml";
import { collectConfigCommands, formatFindings, validateConfigPipeline } from "./pipeline-config.mjs";
import { loadGeneratorRegistry } from "./pipeline-generators.mjs";
import { RIGHT_SUITE_APP_KEYS, RIGHT_SUITE_UPDATER_PUBLIC_KEYS } from "./suite-api.generated.mjs";

export const RELEASE_CONFIG_FILE = "right-release.config.json";
export const LEGACY_RELEASE_CONFIG_FILE = "right-release.config.mjs";
export const RELEASE_CONFIG_SCHEMA = 1;
export const MAX_CONFIG_BYTES = 1024 * 1024;
export const LEGACY_EXECUTABLE_EXTENSIONS = Object.freeze([".mjs", ".js", ".cjs", ".ts", ".mts", ".cts", ".jsx", ".tsx"]);

/** The complete set of top-level keys. A key outside it is refused: a new capability is a RightKit change, not an app extension. */
export const TOP_LEVEL_KEYS = Object.freeze([
  "$schema", "$comment", "schema", "app", "version", "channel", "distribution", "packageManager", "workdir", "targets", "development",
  "deps", "checks", "buildInputs", "hostedWorkflows", "devtools", "releaseSwitches", "qa", "nativeAssembly", "legal", "web", "tests",
  "sidecars", "assembly", "releaseDependencies", "appStoreConnect", "storage", "cargoProfileExempt", "runtimeArtifacts",
]);
export const TEMPLATE_TOKENS = Object.freeze(["app", "version", "home", "localAppData"]);

export class ReleaseConfigError extends Error {
  constructor(code, message, details = {}) {
    super(message);
    this.name = "ReleaseConfigError";
    this.code = code;
    Object.assign(this, details);
  }
}
const fail = (code, message, details) => { throw new ReleaseConfigError(code, message, details); };
const isObject = (value) => value !== null && typeof value === "object" && !Array.isArray(value);

/**
 * Judge a config file NAME. Throws for an executable config and for anything that is not `.json`. It touches no file: call it before
 * opening, importing or even resolving a repository, so a legacy config is refused on every route (dry runs included).
 */
export function assertReleaseConfigFileName(file) {
  if (typeof file !== "string" || !file.trim()) fail("RELEASE_CONFIG_NOT_JSON", "a release config path is required (right-release.config.json)");
  const extension = path.extname(path.basename(file)).toLowerCase();
  if (LEGACY_EXECUTABLE_EXTENSIONS.includes(extension)) {
    fail("RELEASE_CONFIG_LEGACY_EXECUTABLE", `${file} is an executable release config (${extension}). RightKit never imports, evaluates or converts one; write ${RELEASE_CONFIG_FILE} (finite JSON schema, see the README section "Release config (JSON)") and delete the module.`, { file });
  }
  if (extension !== ".json") fail("RELEASE_CONFIG_NOT_JSON", `${file} is not a JSON release config; the only config file is ${RELEASE_CONFIG_FILE}`, { file });
  return file;
}

/** A hint for a missing config: a legacy sibling is named (never opened). */
function missingConfigMessage(file) {
  const legacy = path.join(path.dirname(file), LEGACY_RELEASE_CONFIG_FILE);
  return existsSync(legacy)
    ? `release config not found: ${file}. A legacy executable config exists at ${legacy}; it is never imported or converted. Write ${RELEASE_CONFIG_FILE}.`
    : `release config not found: ${file}`;
}

// ---- strict JSON ----------------------------------------------------------------------------------------------------------------
const FORBIDDEN_KEYS = new Set(["__proto__", "constructor", "prototype"]);
const NUMBER = /-?(?:0|[1-9]\d*)(?:\.\d+)?(?:[eE][+-]?\d+)?/y;
const pointerKey = (key) => key.replaceAll("~", "~0").replaceAll("/", "~1");

/** Parse JSON strictly and remember where every value starts: `{ value, offsets }` (offsets: JSON pointer -> character offset). */
export function parseStrictJson(text) {
  const offsets = new Map();
  let pos = 0;
  const error = (message) => { throw Object.assign(new SyntaxError(message), { offset: pos }); };
  const skip = () => { while (pos < text.length && (text[pos] === " " || text[pos] === "\t" || text[pos] === "\n" || text[pos] === "\r")) pos += 1; };
  const string = () => {
    const start = pos;
    pos += 1;
    while (pos < text.length && text[pos] !== '"') {
      if (text.charCodeAt(pos) < 0x20) error("a control character inside a string");
      pos += text[pos] === "\\" ? 2 : 1;
    }
    if (text[pos] !== '"') error("an unterminated string");
    pos += 1;
    try { return JSON.parse(text.slice(start, pos)); } catch { pos = start; return error("an invalid string escape"); }
  };
  const literal = (word, result) => { if (!text.startsWith(word, pos)) error(`unexpected token`); pos += word.length; return result; };
  const value = (pointer, depth) => {
    if (depth > 64) error("nesting deeper than 64 levels");
    skip();
    offsets.set(pointer, pos);
    const ch = text[pos];
    if (ch === "{") return object(pointer, depth);
    if (ch === "[") return array(pointer, depth);
    if (ch === '"') return string();
    if (ch === "t") return literal("true", true);
    if (ch === "f") return literal("false", false);
    if (ch === "n") return literal("null", null);
    NUMBER.lastIndex = pos;
    const match = NUMBER.exec(text);
    if (!match || match.index !== pos) error("unexpected token");
    pos += match[0].length;
    return Number(match[0]);
  };
  const object = (pointer, depth) => {
    const out = {};
    pos += 1;
    skip();
    if (text[pos] === "}") { pos += 1; return out; }
    for (;;) {
      skip();
      if (text[pos] !== '"') error("expected a string key");
      const keyStart = pos;
      const key = string();
      if (FORBIDDEN_KEYS.has(key)) { pos = keyStart; error(`the key "${key}" is not allowed`); }
      if (Object.prototype.hasOwnProperty.call(out, key)) { pos = keyStart; error(`duplicate key "${key}"`); }
      skip();
      if (text[pos] !== ":") error("expected ':'");
      pos += 1;
      out[key] = value(`${pointer}/${pointerKey(key)}`, depth + 1);
      skip();
      if (text[pos] === ",") { pos += 1; continue; }
      if (text[pos] === "}") { pos += 1; return out; }
      error("expected ',' or '}'");
    }
  };
  const array = (pointer, depth) => {
    const out = [];
    pos += 1;
    skip();
    if (text[pos] === "]") { pos += 1; return out; }
    for (;;) {
      out.push(value(`${pointer}/${out.length}`, depth + 1));
      skip();
      if (text[pos] === ",") { pos += 1; continue; }
      if (text[pos] === "]") { pos += 1; return out; }
      error("expected ',' or ']'");
    }
  };
  const result = value("", 0);
  skip();
  if (pos !== text.length) error("unexpected content after the JSON value");
  return { value: result, offsets };
}

/** 1-based line of a JSON pointer in `text` (the nearest recorded ancestor when the exact pointer is not a source position). */
export function lineOfPointer(text, offsets, pointer) {
  let current = pointer;
  while (current !== "" && !offsets.has(current)) current = current.slice(0, current.lastIndexOf("/"));
  const offset = offsets.get(current) ?? 0;
  let line = 1;
  for (let i = 0; i < offset && i < text.length; i += 1) if (text[i] === "\n") line += 1;
  return line;
}

// ---- templates ------------------------------------------------------------------------------------------------------------------
function readInside(appRoot, relative, label, { readBytes, lstat }) {
  if (typeof relative !== "string" || !relative.trim() || path.isAbsolute(relative) || path.win32.isAbsolute(relative) || /^[A-Za-z]:/.test(relative) || relative.replaceAll("\\", "/").split("/").includes("..")) {
    fail("RELEASE_CONFIG_TEMPLATE", `${label}: file must be a relative path inside the app: ${JSON.stringify(relative)}`);
  }
  const absolute = path.resolve(appRoot, relative);
  let info;
  try { info = lstat(absolute); } catch { fail("RELEASE_CONFIG_TEMPLATE", `${label}: ${relative} does not exist`); }
  if (info.isSymbolicLink() || !info.isFile()) fail("RELEASE_CONFIG_TEMPLATE", `${label}: ${relative} must be a regular file (no symlink)`);
  if (info.size > MAX_CONFIG_BYTES) fail("RELEASE_CONFIG_TEMPLATE", `${label}: ${relative} is larger than ${MAX_CONFIG_BYTES} bytes`);
  const bytes = readBytes(absolute);
  return { bytes, sha256: createHash("sha256").update(bytes).digest("hex"), relative: relative.replaceAll("\\", "/") };
}

function jsonPointerGet(document, pointer, label) {
  if (pointer === "") return document;
  if (typeof pointer !== "string" || !pointer.startsWith("/")) fail("RELEASE_CONFIG_TEMPLATE", `${label}: pointer must be a JSON pointer such as /version`);
  let current = document;
  for (const raw of pointer.slice(1).split("/")) {
    const key = raw.replaceAll("~1", "/").replaceAll("~0", "~");
    if (!(isObject(current) || Array.isArray(current)) || !Object.prototype.hasOwnProperty.call(current, key)) fail("RELEASE_CONFIG_TEMPLATE", `${label}: ${pointer} is not in the file`);
    current = current[key];
  }
  return current;
}

function scalarString(value, label) {
  if (typeof value === "string") return value;
  if (typeof value === "number" && Number.isFinite(value)) return String(value);
  return fail("RELEASE_CONFIG_TEMPLATE", `${label}: the referenced value must be a string or a number`);
}

/** Replace the typed templates in `node`. `context` carries the template values, the app root and the injectable file access. */
function expand(node, context, label) {
  if (typeof node === "string") {
    return node.replace(/\{\{([A-Za-z][A-Za-z0-9]*)\}\}/g, (_, token) => {
      if (!TEMPLATE_TOKENS.includes(token)) fail("RELEASE_CONFIG_TEMPLATE", `${label}: unknown template token {{${token}}} (allowed: ${TEMPLATE_TOKENS.map((t) => `{{${t}}}`).join(", ")})`);
      const resolved = context.values[token];
      if (typeof resolved !== "string" || resolved === "") fail("RELEASE_CONFIG_TEMPLATE", `${label}: {{${token}}} is not available${token === "version" ? " here (version cannot refer to itself)" : ""}`);
      return resolved;
    });
  }
  if (Array.isArray(node)) return node.map((item, index) => expand(item, context, `${label}/${index}`));
  if (!isObject(node)) return node;
  const keys = Object.keys(node);
  if (keys.length === 1 && keys[0] === "$fromJson") {
    const spec = node.$fromJson;
    if (!isObject(spec) || Object.keys(spec).some((key) => key !== "file" && key !== "pointer") || typeof spec.file !== "string") fail("RELEASE_CONFIG_TEMPLATE", `${label}: $fromJson must be { file, pointer }`);
    const read = readInside(context.appRoot, spec.file, `${label} $fromJson`, context);
    let document;
    try { document = JSON.parse(Buffer.from(read.bytes).toString("utf8")); } catch { fail("RELEASE_CONFIG_TEMPLATE", `${label}: ${spec.file} is not valid JSON`); }
    context.inputs.set(read.relative, read.sha256);
    return scalarString(jsonPointerGet(document, spec.pointer ?? "", `${label} $fromJson`), label);
  }
  if (keys.length === 1 && keys[0] === "$fromToml") {
    const spec = node.$fromToml;
    if (!isObject(spec) || Object.keys(spec).some((key) => key !== "file" && key !== "path") || typeof spec.file !== "string" || typeof spec.path !== "string") fail("RELEASE_CONFIG_TEMPLATE", `${label}: $fromToml must be { file, path }`);
    const read = readInside(context.appRoot, spec.file, `${label} $fromToml`, context);
    let document;
    try { document = parseToml(Buffer.from(read.bytes).toString("utf8")); } catch { fail("RELEASE_CONFIG_TEMPLATE", `${label}: ${spec.file} is not valid TOML`); }
    context.inputs.set(read.relative, read.sha256);
    let current = document;
    for (const key of spec.path.split(".")) {
      if (!isObject(current) || !Object.prototype.hasOwnProperty.call(current, key)) fail("RELEASE_CONFIG_TEMPLATE", `${label}: ${spec.path} is not in ${spec.file}`);
      current = current[key];
    }
    return scalarString(current, label);
  }
  if (keys.length === 1 && keys[0] === "$json") return JSON.stringify(expand(node.$json, context, `${label}/$json`));
  const out = {};
  for (const key of keys) {
    if (key.startsWith("$") && !TOP_LEVEL_KEYS.includes(key)) fail("RELEASE_CONFIG_TEMPLATE", `${label}/${key}: "$" keys other than $fromJson, $fromToml and $json are reserved`);
    out[key] = expand(node[key], context, `${label}/${pointerKey(key)}`);
  }
  return out;
}

// ---- shape ----------------------------------------------------------------------------------------------------------------------
const SAFE_APP = /^[A-Za-z0-9][A-Za-z0-9._-]*$/;
const SAFE_VERSION = /^[A-Za-z0-9][A-Za-z0-9._+-]*$/;

/** Schema, finite key set and the identity fields. Throws RELEASE_CONFIG_SCHEMA / RELEASE_CONFIG_KEY. */
export function validateReleaseConfigShape(config, { file = RELEASE_CONFIG_FILE, phase = "final" } = {}) {
  if (!isObject(config)) fail("RELEASE_CONFIG_SCHEMA", `${file} must be a JSON object`, { file });
  if (config.schema !== RELEASE_CONFIG_SCHEMA) fail("RELEASE_CONFIG_SCHEMA", `${file}: unsupported config schema ${JSON.stringify(config.schema ?? null)} (expected ${RELEASE_CONFIG_SCHEMA})`, { file });
  const unknown = Object.keys(config).filter((key) => !TOP_LEVEL_KEYS.includes(key));
  if (unknown.length) fail("RELEASE_CONFIG_KEY", `${file}: unknown top-level key(s): ${unknown.join(", ")}. The config schema is finite (allowed: ${TOP_LEVEL_KEYS.filter((key) => !key.startsWith("$")).join(", ")}); a new capability is a RightKit change.`, { file, unknown });
  if (typeof config.app !== "string" || !SAFE_APP.test(config.app)) fail("RELEASE_CONFIG_SCHEMA", `${file}: app must be a safe name (letters, digits, . _ -)`, { file });
  // Before template expansion a version may still be a { $fromJson | $fromToml } node or carry a {{token}}; the final check is strict.
  const versionIsTemplate = phase === "raw" && (isObject(config.version) || (typeof config.version === "string" && config.version.includes("{{")));
  if (config.version !== undefined && !versionIsTemplate && (typeof config.version !== "string" || !SAFE_VERSION.test(config.version))) fail("RELEASE_CONFIG_SCHEMA", `${file}: version must be a safe version string`, { file });
  for (const key of ["targets", "development", "buildInputs", "distribution"]) {
    if (config[key] !== undefined && !isObject(config[key])) fail("RELEASE_CONFIG_SCHEMA", `${file}: ${key} must be an object`, { file });
  }
  const fieldProblems = releaseConfigFieldProblems(config);
  if (fieldProblems.length) fail(fieldProblems[0].code, fieldProblems.map((problem) => `${file}: ${problem.detail}`).join("\n"), { file, pointers: fieldProblems.map((problem) => problem.pointer) });
  return config;
}

// The same rule as UPDATER_KEY_ID_RE in github-updater-release.mjs, kept here because that module's import graph loads the release state
// machine, which this loader must not depend on.
const UPDATER_KEY_ID = /^[0-9A-F]{16}$/;
// A typed template ({ "$fromJson": ... }) is a string only after expansion, so the raw-phase check leaves it to the final one.
const isTemplateNode = (value) => isObject(value) && Object.keys(value).some((key) => key.startsWith("$"));
const isBlank = (value) => value === undefined || value === null || (typeof value === "string" && !value.trim());

/**
 * Field-level refusals that need no other part of the config, so a caller (the loader, the doctor) can list them next to every other
 * finding: `[{ name, code, pointer, detail }]`.
 *
 * `targets.<t>.updater.keyFile`: updater minisign keys come from the OS credential provider (`protected-credentials.mjs`, secret
 * "updater-signing-key", namespace `rightsuite-updater-key`; `sign-updater.mjs` refuses `--key-file`), so a config that names a key
 * file could only fail mid-signing. It is refused up front instead, and never forwarded.
 *
 * `distribution.updaterKeyId` must be 16 upper-case hex digits and, for a Right Suite app, a key the Right Suite contract accepts; a
 * Right Suite app must not declare an updater public key of its own (`distribution.updaterPublicKey`, `updaterPubkey`,
 * `targets.<t>.updater.pubkey` or `publicKey`) unless it is exactly a contract key's text. Publish and seal refuse the same things
 * (`resolveUpdaterTrust`); here they are found before a build starts.
 */
export function releaseConfigFieldProblems(config) {
  const problems = [];
  if (!isObject(config)) return problems;
  const targets = isObject(config.targets) ? Object.entries(config.targets) : [];
  for (const [name, target] of targets) {
    if (!isObject(target) || !isObject(target.updater) || !Object.prototype.hasOwnProperty.call(target.updater, "keyFile")) continue;
    problems.push({
      name: "updater-key-file",
      code: "RELEASE_CONFIG_UPDATER_KEY_FILE",
      pointer: `/targets/${pointerKey(name)}/updater/keyFile`,
      detail: `targets.${name}.updater.keyFile is not supported: updater signing keys come from the OS credential provider (rightkit-credentials, namespace rightsuite-updater-key; see protected-credentials.mjs and sign-updater.mjs), never from a key file. Remove updater.keyFile from the release config.`,
    });
  }
  const suiteApp = typeof config.app === "string" && RIGHT_SUITE_APP_KEYS.includes(config.app.toLowerCase());
  const distribution = isObject(config.distribution) ? config.distribution : {};
  const keyId = distribution.updaterKeyId;
  if (keyId !== undefined && !isTemplateNode(keyId)) {
    const accepted = RIGHT_SUITE_UPDATER_PUBLIC_KEYS.map((key) => key.kid);
    const detail = typeof keyId !== "string" || !UPDATER_KEY_ID.test(keyId)
      ? `distribution.updaterKeyId must be the 16 upper-case hex digits of a minisign key id, got ${JSON.stringify(keyId)}.`
      : suiteApp && !accepted.includes(keyId)
        ? `distribution.updaterKeyId ${keyId} is not an updater key in the Right Suite contract (accepted: ${accepted.join(", ")}). Remove it to sign with the contract's signing key.`
        : null;
    if (detail) problems.push({ name: "updater-key-id", code: "RELEASE_CONFIG_UPDATER_KEY_ID", pointer: "/distribution/updaterKeyId", detail });
  }
  if (suiteApp) {
    const contractTexts = RIGHT_SUITE_UPDATER_PUBLIC_KEYS.map((key) => key.publicKey);
    const declared = (pointer, label, value) => {
      if (isBlank(value) || isTemplateNode(value) || contractTexts.includes(value)) return;
      problems.push({
        name: "updater-public-key",
        code: "RELEASE_CONFIG_UPDATER_PUBLIC_KEY",
        pointer,
        detail: `${label} is not a Right Suite contract key: ${config.app} is a suite app and takes its updater keys from the contract (contracts/right-suite-api.v1.json), so remove ${label}.`,
      });
    };
    for (const field of ["updaterPublicKey", "updaterPubkey"]) declared(`/distribution/${field}`, `distribution.${field}`, distribution[field]);
    for (const [name, target] of targets) {
      if (!isObject(target) || !isObject(target.updater)) continue;
      for (const field of ["pubkey", "publicKey"]) declared(`/targets/${pointerKey(name)}/updater/${field}`, `targets.${name}.updater.${field}`, target.updater[field]);
    }
  }
  return problems;
}

function packageScripts(appRoot, readText) {
  try {
    const parsed = JSON.parse(readText(path.join(appRoot, "package.json")));
    return isObject(parsed?.scripts) ? parsed.scripts : undefined;
  } catch { return undefined; }
}

/**
 * Load, validate and expand a release config. Returns `{ config, configPath, appRoot, sha256, inputs, lineOf }`. Throws ReleaseConfigError
 * (RELEASE_CONFIG_LEGACY_EXECUTABLE | _NOT_JSON | _NOT_FOUND | _INVALID_JSON | _SCHEMA | _KEY | _UPDATER_KEY_FILE | _UPDATER_KEY_ID | _UPDATER_PUBLIC_KEY | _TEMPLATE | _M24). `validatePipeline: false`
 * skips only the M24 command admission (the pipeline scanner reports those findings itself); nothing else is optional.
 * File access, the generator registry, the environment and the home directory are injectable for tests.
 */
export function loadReleaseConfigSync(file, {
  validatePipeline = true, registry, containment, env = process.env, homedir = () => os.homedir(),
  readText = (target) => readFileSync(target, "utf8"), readBytes = (target) => readFileSync(target), lstat = lstatSync,
} = {}) {
  assertReleaseConfigFileName(file);
  const configPath = path.resolve(file);
  const appRoot = path.dirname(configPath);
  let text;
  try { text = readText(configPath); } catch { fail("RELEASE_CONFIG_NOT_FOUND", missingConfigMessage(configPath), { file: configPath }); }
  if (Buffer.byteLength(text) > MAX_CONFIG_BYTES) fail("RELEASE_CONFIG_INVALID_JSON", `${configPath} is larger than ${MAX_CONFIG_BYTES} bytes`, { file: configPath });
  if (text.charCodeAt(0) === 0xfeff) text = text.slice(1);
  let parsed;
  try { parsed = parseStrictJson(text); } catch (error) {
    let line = 1;
    for (let i = 0; i < (error.offset ?? 0) && i < text.length; i += 1) if (text[i] === "\n") line += 1;
    fail("RELEASE_CONFIG_INVALID_JSON", `${configPath}:${line} is not valid strict JSON (${error.message}; no comments, trailing commas or duplicate keys)`, { file: configPath, line });
  }
  const raw = parsed.value;
  validateReleaseConfigShape(raw, { file: configPath, phase: "raw" });
  const inputs = new Map();
  // {{localAppData}} is %LOCALAPPDATA% on Windows. Elsewhere it is the Windows profile default under the home directory (a path
  // no other host uses), so one config that names a Windows install path loads on every host instead of failing on the Mac.
  const base = { appRoot, inputs, readBytes, lstat, values: { home: homedir(), localAppData: env.LOCALAPPDATA || path.join(homedir(), "AppData", "Local"), app: "", version: "" } };
  base.values.app = expand(raw.app, { ...base }, "/app");
  if (raw.version !== undefined) base.values.version = expand(raw.version, { ...base, values: { ...base.values, version: "" } }, "/version");
  const config = expand(raw, base, "");
  validateReleaseConfigShape(config, { file: configPath });
  const sha256 = createHash("sha256").update(Buffer.from(text)).digest("hex");
  if (validatePipeline) {
    const findings = validateConfigPipeline(config, {
      scripts: packageScripts(appRoot, readText),
      registry: registry ?? loadGeneratorRegistry(),
      appRoot,
      containment,
      readBytes,
      lineOf: (pointer) => lineOfPointer(text, parsed.offsets, pointer),
    });
    if (findings.length) {
      fail("RELEASE_CONFIG_M24", `${configPath} has ${findings.length} M24 pipeline finding(s); every command must be an admitted RightKit invocation:\n${formatFindings(findings, configPath).map((line) => `- ${line}`).join("\n")}`, { file: configPath, findings });
    }
  }
  return {
    config, configPath, appRoot, sha256,
    inputs: [...inputs].map(([file, digest]) => ({ file, sha256: digest })),
    lineOf: (pointer) => lineOfPointer(text, parsed.offsets, pointer),
  };
}

/** The config object only (what `(await import(configPath)).default` used to be). */
export async function loadReleaseConfig(file, options) {
  return loadReleaseConfigSync(file, options).config;
}

/** True for a `.json` config path: the only extension the strict loader accepts. */
export const isJsonReleaseConfig = (file) => typeof file === "string" && path.extname(file).toLowerCase() === ".json";

/** The config name a command defaults to: `right-release.config.json` when `dir` holds it, else the legacy module an unmoved app still owns. */
export function defaultReleaseConfigFile(dir = process.cwd(), { exists = existsSync } = {}) {
  return exists(path.join(dir, RELEASE_CONFIG_FILE)) ? RELEASE_CONFIG_FILE : LEGACY_RELEASE_CONFIG_FILE;
}

/**
 * Load the config at `file` for a command. A `.json` path goes through the strict, M24-validated loader (throws ReleaseConfigError); any
 * other path is the legacy branch and is imported as before. Legacy-only options: `query` is the cache-busting query a site appends
 * (`upload=<ts>`, `commit=<sha>`) so a module re-evaluates per run, and `lenient` keeps the `default ?? config ?? module` fallback the
 * app-root commands accept; every other option goes to `loadReleaseConfigSync`.
 */
export async function importReleaseConfig(file, { query, lenient = false, ...loadOptions } = {}) {
  const absolute = path.resolve(file);
  if (isJsonReleaseConfig(absolute)) return loadReleaseConfigSync(absolute, loadOptions).config;
  const loaded = await import(`${pathToFileURL(absolute).href}${query ? `?${query}` : ""}`);
  return lenient ? loaded.default ?? loaded.config ?? loaded : loaded.default;
}

/** `{ config, appRoot, configPath }` for the commands that need the app root beside the config. */
export async function loadAppConfigFile(file = RELEASE_CONFIG_FILE, options) {
  const loaded = loadReleaseConfigSync(file, options);
  return { config: loaded.config, appRoot: loaded.appRoot, configPath: loaded.configPath };
}

export { collectConfigCommands };
