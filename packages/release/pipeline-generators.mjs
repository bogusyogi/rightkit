// M24 R4: the typed shared-generator registry.
//
// Product-source generation (an icon pass, a schema codegen, an engine stage) may sit inside a pipeline only as an explicitly admitted
// shared step. Each registry entry binds one generator to its script, its executable dependency closure (every file the script loads),
// its interpreter, its exact argument vector and its I/O limits, by sha256. An app cannot register one: the registry is a RightKit file
// (`pipeline-generators.json`), so a new capability is a RightKit change. An admitted generator can reach no network and perform no
// signing, installation or upload; the registry refuses an entry that claims otherwise, and the static check below refuses a
// generator source that names a pipeline tool or dispatches RightKit again (a generator may never start another pipeline).
//
// Static review is NOT a sandbox. The process, network and output limits are enforced by native containment, which this package does
// not yet implement for any platform: `detectContainment` reports it unsupported, and an unsupported containment REFUSES the
// generator (the target stays unqualified) instead of running it unconfined. Callers inject a real containment to admit one.
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { dispatchesRightKit, scanTextForPipelineTools, segmentTokens, splitSegments } from "./pipeline-tools.mjs";

const here = path.dirname(fileURLToPath(import.meta.url));
export const GENERATOR_REGISTRY_FILE = "pipeline-generators.json";
export const GENERATOR_INTERPRETERS = Object.freeze(["node"]);
const SHA256 = /^[0-9a-f]{64}$/;
const ID = /^[a-z][a-z0-9-]{1,63}$/;
const APP = /^[A-Za-z0-9][A-Za-z0-9._-]*$/;
const ENTRY_KEYS = ["id", "app", "interpreter", "script", "scriptSha256", "args", "closure", "inputs", "outputs", "limits", "network"];
const LIMIT_KEYS = ["maxOutputBytes", "maxProcesses", "timeoutMs"];
const isObject = (value) => value !== null && typeof value === "object" && !Array.isArray(value);

export class GeneratorRegistryError extends Error {
  constructor(message) { super(message); this.name = "GeneratorRegistryError"; this.code = "RK_GENERATOR_REGISTRY"; }
}
const bad = (message) => { throw new GeneratorRegistryError(message); };

function relativePath(value, label) {
  if (typeof value !== "string" || !value.trim() || path.isAbsolute(value) || path.win32.isAbsolute(value) || /^[A-Za-z]:/.test(value) || value.includes("\\")) bad(`${label} must be a forward-slash relative path`);
  const parts = value.replace(/^(?:\.\/)+/, "").split("/");
  if (parts.includes("..") || parts.includes("") || parts.includes(".")) bad(`${label} must stay inside the app: ${value}`);
  return parts.join("/");
}

/** Validate one registry entry; returns the normalised entry. */
export function validateGeneratorEntry(entry, label = "generator") {
  if (!isObject(entry)) bad(`${label} must be an object`);
  const unknown = Object.keys(entry).filter((key) => !ENTRY_KEYS.includes(key));
  if (unknown.length) bad(`${label} has unknown key(s): ${unknown.join(", ")}`);
  if (typeof entry.id !== "string" || !ID.test(entry.id)) bad(`${label}.id must be a lowercase id`);
  if (typeof entry.app !== "string" || !APP.test(entry.app)) bad(`${label}.app must name the registered app`);
  if (!GENERATOR_INTERPRETERS.includes(entry.interpreter)) bad(`${label}.interpreter must be one of ${GENERATOR_INTERPRETERS.join(", ")}`);
  const script = relativePath(entry.script, `${label}.script`);
  if (typeof entry.scriptSha256 !== "string" || !SHA256.test(entry.scriptSha256)) bad(`${label}.scriptSha256 must be a lowercase sha256`);
  if (!Array.isArray(entry.args) || entry.args.some((arg) => typeof arg !== "string" || /[\0\r\n]/.test(arg))) bad(`${label}.args must be the exact argument vector (a list of strings)`);
  if (!Array.isArray(entry.closure)) bad(`${label}.closure must list every executable dependency of the script (an empty list for none)`);
  const closure = entry.closure.map((item, index) => {
    if (!isObject(item) || Object.keys(item).some((key) => key !== "path" && key !== "sha256")) bad(`${label}.closure[${index}] must be { path, sha256 }`);
    if (typeof item.sha256 !== "string" || !SHA256.test(item.sha256)) bad(`${label}.closure[${index}].sha256 must be a lowercase sha256`);
    return { path: relativePath(item.path, `${label}.closure[${index}].path`), sha256: item.sha256 };
  });
  for (const key of ["inputs", "outputs"]) {
    if (!Array.isArray(entry[key]) || entry[key].length === 0) bad(`${label}.${key} must be a non-empty list of app-relative globs (the I/O limit)`);
    for (const [index, glob] of entry[key].entries()) {
      if (typeof glob !== "string" || glob.includes("..") || path.isAbsolute(glob) || /^[A-Za-z]:/.test(glob) || glob.includes("\\")) bad(`${label}.${key}[${index}] must be an app-relative forward-slash glob`);
    }
  }
  if (!isObject(entry.limits) || Object.keys(entry.limits).some((key) => !LIMIT_KEYS.includes(key))) bad(`${label}.limits must be an object with ${LIMIT_KEYS.join(", ")}`);
  for (const key of LIMIT_KEYS) if (!Number.isInteger(entry.limits[key]) || entry.limits[key] <= 0) bad(`${label}.limits.${key} must be a positive integer`);
  if (entry.network !== false) bad(`${label}.network must be false: an admitted generator reaches no network, and cannot sign, install or upload`);
  return { ...entry, script, closure, args: [...entry.args], inputs: [...entry.inputs], outputs: [...entry.outputs] };
}

/** Load and validate the registry. A missing or malformed file throws: the shipped registry always exists (empty means no generator is admitted). */
export function loadGeneratorRegistry(file = path.join(here, GENERATOR_REGISTRY_FILE), { readText = (target) => readFileSync(target, "utf8") } = {}) {
  let parsed;
  try { parsed = JSON.parse(readText(file)); } catch (error) { bad(`the generator registry ${file} cannot be read: ${error.message}`); }
  if (!isObject(parsed) || parsed.schema !== 1 || !Array.isArray(parsed.generators)) bad(`${file} must be { schema: 1, generators: [...] }`);
  const seen = new Set();
  const generators = parsed.generators.map((entry, index) => {
    const valid = validateGeneratorEntry(entry, `generators[${index}]`);
    if (seen.has(valid.id)) bad(`generators[${index}] repeats the id ${valid.id}`);
    seen.add(valid.id);
    return valid;
  });
  return { schema: 1, generators };
}

/** Native containment for a generator (no network, bounded output and processes). None is implemented yet: report it, never fake it. */
export function detectContainment({ platform = process.platform } = {}) {
  return { supported: false, mechanism: null, reason: `no native containment (network-, output- and process-restricted) is implemented for ${platform}; the generator is refused and the target stays unqualified` };
}

const sha256File = (file, readBytes) => createHash("sha256").update(readBytes(file)).digest("hex");

/** Static refusals on a generator file's text: a pipeline tool, or RightKit dispatched again. */
export function generatorSourceProblems(text, label) {
  const problems = [];
  for (const hit of scanTextForPipelineTools(text, { kind: "code" })) problems.push(`${label}:${hit.line} runs ${hit.tool} (${hit.stage}); a generator cannot sign, install or upload`);
  for (const [index, line] of String(text).split(/\r?\n/).entries()) {
    for (const literal of line.matchAll(/["'`]([^"'`\n]{1,200})["'`]/g)) {
      const tokens = segmentTokens(splitSegments(literal[1])[0] ?? "");
      if (tokens.length && dispatchesRightKit(tokens)) problems.push(`${label}:${index + 1} dispatches RightKit; a generator cannot start another pipeline`);
    }
  }
  return problems;
}

/**
 * Is `node <script> <args...>` an admitted generator for `app`? Returns `{ admitted: true, id }` or `{ admitted: false, code, reason }`.
 * Revalidates the files at the moment of the call (a hash recorded earlier is not enough). All file access and the containment
 * probe are injectable.
 */
export function admitGeneratorScript({ registry, app, appRoot, script, args = [], containment = detectContainment, readBytes = (file) => readFileSync(file), readText = (file) => readFileSync(file, "utf8") }) {
  const refuse = (code, reason) => ({ admitted: false, code, reason });
  const entries = (registry?.generators ?? []).filter((entry) => entry.app === app && entry.script === script);
  if (entries.length === 0) return refuse("R4-unregistered-generator", `no registered generator for ${app} runs ${script}; apps cannot self-register, a new generator is a RightKit change`);
  const entry = entries.find((candidate) => candidate.args.length === args.length && candidate.args.every((arg, index) => arg === args[index]));
  if (!entry) return refuse("R4-arguments", `${script}: the argument vector differs from the registered one; only the exact registered arguments are admitted`);
  const contained = containment();
  if (!contained?.supported) return refuse("R4-containment-unsupported", contained?.reason ?? "native containment is not available");
  const files = [{ path: entry.script, sha256: entry.scriptSha256 }, ...entry.closure];
  for (const file of files) {
    const absolute = path.resolve(appRoot, file.path);
    let digest;
    try { digest = sha256File(absolute, readBytes); } catch { return refuse("R4-missing-file", `${file.path} (registered for ${entry.id}) cannot be read`); }
    if (digest !== file.sha256) return refuse("R4-digest-mismatch", `${file.path} does not match the registered digest for ${entry.id}; the generator changed after review`);
    if (/\.(?:[cm]?js|ts)$/i.test(file.path)) {
      let text;
      try { text = readText(absolute); } catch { return refuse("R4-missing-file", `${file.path} cannot be read`); }
      const problems = generatorSourceProblems(text, file.path);
      if (problems.length) return refuse("R4-forbidden-effect", problems.join("; "));
    }
  }
  return { admitted: true, id: entry.id, interpreter: entry.interpreter, limits: entry.limits, containment: contained.mechanism };
}

/** The `admitGenerator` option for `admitInvocation`, bound to one app. */
export function makeGeneratorAdmitter({ registry, app, appRoot, ...injected }) {
  return ({ script, args }) => admitGeneratorScript({ registry, app, appRoot, script, args, ...injected });
}
