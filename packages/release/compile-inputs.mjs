// The declared compile inputs of a gate snapshot: the ignored paths (a built `dist`, generated assets, a Windows identity package) the commit does not hold,
// listed in the snapshot's `.rustright/compile-inputs.json` (or the legacy `.rightkit/compile-inputs.json`) and produced into the snapshot by the gate's own producer
// steps. The gate reads them from the snapshot and judges exactly those bytes (judgeCompileInputs). The broker captures no declared input (the seam carries none:
// docs/BUILD.md section 2 "Request", reset plan section 4 row `stage-ledger.mjs, compile-inputs.mjs`), so there is no broker-captured tree to judge here.
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { spawnSync } from "./background-process.mjs";
import { SECRET_PATTERNS } from "./publish-cargo.mjs";

// ---- the declared compile inputs, read and digested the way the broker does (moved here from snapshot-request.mjs, deleted at plan step 5) ----
/**
 * Where the declared compile inputs are listed, as read from the snapshot (the broker's COMPILE_INPUTS; GB-49): `.rustright/compile-inputs.json` first,
 * `.rightkit/compile-inputs.json` ONLY when the canonical file is absent. A present but invalid canonical declaration is an error, never a fallback.
 */
export const COMPILE_INPUTS_CANONICAL = ".rustright/compile-inputs.json";
export const COMPILE_INPUTS_LEGACY = ".rightkit/compile-inputs.json";
export const COMPILE_INPUT_DECLARATIONS = Object.freeze([COMPILE_INPUTS_CANONICAL, COMPILE_INPUTS_LEGACY]);
export const MAX_DECLARATION_BYTES = 4 * 1024 * 1024;

export class SnapshotRequestError extends Error {
  constructor(code, message) { super(message); this.name = "SnapshotRequestError"; this.code = code; }
}
const fail = (code, message) => { throw new SnapshotRequestError(code, message); };


// ---- declared compile inputs, read and digested the way the broker does (check_admission.rs declared_inputs / snapshot_inputs) ----

const lstatOrNull = (p) => { try { return fs.lstatSync(p); } catch (e) { if (e.code === "ENOENT") return null; throw e; } };

/**
 * The declared inputs of a snapshot, read FROM THE SNAPSHOT, never the checkout, by the one rule every RightKit reader shares (the broker's, vector
 * docs/contracts/declared-inputs.fixtures.json): `.rustright/compile-inputs.json` when it exists, else `.rightkit/compile-inputs.json`, else none. Either file
 * is judged by the same checks: no link on its path, a regular file of at most 4 MiB, JSON with `inputs` a list of plain relative paths (not absolute, no `.`,
 * `..` or empty component, not inside `.git`) and no input that is, or holds, either declaration file. A present but invalid canonical declaration is an
 * error: it is never read as the legacy one.
 */
/** Host platform names a declared input may scope itself to (`{ path, platforms }`). */
export const DECLARED_INPUT_PLATFORMS = Object.freeze({ darwin: "mac", win32: "win", linux: "linux" });

export function readDeclaredInputs(snapshotRoot, { platform = process.platform } = {}) {
  let file = null; let at = null; let meta = null;
  for (const candidate of COMPILE_INPUT_DECLARATIONS) {
    let here = snapshotRoot; let found = true;
    for (const part of candidate.split("/")) {
      here = path.join(here, part);
      const m = lstatOrNull(here);
      if (m === null) { found = false; break; }
      if (m.isSymbolicLink()) fail("declared-inputs", `${candidate} runs through a link at ${here}`);
      meta = m;
    }
    if (found) { file = candidate; at = here; break; }
  }
  if (file === null) return [];
  if (!meta.isFile()) fail("declared-inputs", `${file} is not a regular file`);
  if (meta.size > MAX_DECLARATION_BYTES) fail("declared-inputs", `${file} is larger than ${MAX_DECLARATION_BYTES} bytes`);
  let value;
  try { value = JSON.parse(fs.readFileSync(at, "utf8")); } catch (e) { fail("declared-inputs", `${file} is not JSON (${e.message})`); }
  if (!value || typeof value !== "object" || !Array.isArray(value.inputs)) fail("declared-inputs", `${file}: \`inputs\` must be a list of paths`);
  // An entry is a path (every platform) or { path, platforms }: a platform-scoped input (a Windows-only identity package) is declared
  // only on the hosts it names, so another host neither requires nor digests it. Unknown keys or platform names refuse.
  const host = DECLARED_INPUT_PLATFORMS[platform] ?? platform;
  const kept = [];
  for (const entry of value.inputs) {
    let input = entry;
    if (entry && typeof entry === "object" && !Array.isArray(entry)) {
      const extra = Object.keys(entry).filter((k) => k !== "path" && k !== "platforms");
      if (extra.length) fail("declared-inputs", `${file}: an input object has only path and platforms (found ${extra.join(", ")})`);
      const names = Object.values(DECLARED_INPUT_PLATFORMS);
      if (!Array.isArray(entry.platforms) || !entry.platforms.length || entry.platforms.some((x) => !names.includes(x))) fail("declared-inputs", `${file}: ${JSON.stringify(entry.path)}: platforms must be a non-empty list of ${names.join(", ")}`);
      input = entry.path;
    }
    if (typeof input !== "string" || !input) fail("declared-inputs", `${file}: every input is a non-empty relative path`);
    const here = !(entry && typeof entry === "object") || entry.platforms.includes(host);
    const components = input.split(/[\\/]/);
    if (path.isAbsolute(input) || path.posix.isAbsolute(input) || path.win32.isAbsolute(input) || components.some((c) => c === "" || c === "." || c === "..")) fail("declared-inputs", `${file}: ${input} is not a plain relative path inside the snapshot`);
    if (components.some((c) => c.toLowerCase() === ".git")) fail("declared-inputs", `${file}: ${input} is inside .git; repository metadata is never a compile input`);
    for (const declaration of COMPILE_INPUT_DECLARATIONS) {
      const parts = declaration.split("/");
      if (components.length <= parts.length && components.every((c, i) => c === parts[i])) fail("declared-inputs", `${file}: ${input} overlaps the declaration file itself (${declaration})`);
    }
    if (here) kept.push(input);
  }
  return kept;
}


/** The canonical declaration (GB-49); the legacy `.rightkit/compile-inputs.json` is read only when it is absent (readDeclaredInputs below, the one reader). */
export const COMPILE_INPUTS_FILE = COMPILE_INPUTS_CANONICAL;

/** Files of one declared input in the broker's order (children by name), refusing links; [{ name, abs }]. */
function inputFiles(root, input) {
  const out = [];
  const walk = (abs, name) => {
    const st = fs.lstatSync(abs);
    if (st.isSymbolicLink()) throw new Error(`declared compile input holds a symbolic link at ${name}`);
    if (st.isDirectory()) {
      for (const child of fs.readdirSync(abs).sort((a, b) => Buffer.compare(Buffer.from(a), Buffer.from(b)))) walk(path.join(abs, child), `${name}/${child}`);
    } else if (st.isFile()) out.push({ name, abs });
  };
  walk(path.join(root, input), input);
  return out;
}

/**
 * Judges the declared inputs under `root` (the gate's snapshot): each declared path must exist without links; every `.js/.mjs/.cjs`
 * file parses (`node --check`), every `.json` file parses, and no file carries a secret. Returns { inputs: [{ path, sha256,
 * files }], findings: [string], missing: [string], malformed: string|null }: `sha256` is computed over each file's name, NUL, bytes, NUL in order, so a
 * ledger cross-check (stage-ledger.mjs declaredInputProblems) and the direct receipt name the same bytes; `missing` is the subset of `findings` that
 * only say a declared path is not in the tree; `malformed` is the declaration's own problem (the run refuses on it).
 */
export function judgeCompileInputs(root, { spawn = spawnSync, execPath = process.execPath } = {}) {
  const findings = []; const missing = [];
  // One reader for every consumer: canonical first, legacy only when the canonical file is absent, shared link, size, path and overlap checks.
  let declared;
  try { declared = readDeclaredInputs(root); } catch (e) { if (e instanceof SnapshotRequestError) return { inputs: [], findings: [e.message], missing, malformed: e.message }; throw e; }
  if (declared.length === 0) return { inputs: [], findings, missing, malformed: null };
  const inputs = [];
  for (const input of declared) {
    let files;
    try { files = inputFiles(root, input.split(/[\\/]/).join("/")); } catch (e) { const finding = `${input}: ${e.code === "ENOENT" ? "missing from the snapshot" : e.message}`; findings.push(finding); if (e.code === "ENOENT") missing.push(finding); continue; }
    const hash = crypto.createHash("sha256");
    for (const f of files) {
      const bytes = fs.readFileSync(f.abs);
      hash.update(f.name); hash.update(Buffer.from([0])); hash.update(bytes); hash.update(Buffer.from([0]));
      const text = bytes.toString("utf8");
      if (SECRET_PATTERNS.some((pattern) => pattern.test(text))) findings.push(`${f.name}: holds a secret (token or private key pattern)`);
      if (/\.(m|c)?js$/.test(f.name)) {
        const r = spawn(execPath, ["--check", f.abs], { encoding: "utf8" });
        if (r.status !== 0) findings.push(`${f.name}: does not parse (${String(r.stderr ?? "").split("\n").find((l) => /Error/.test(l))?.trim() ?? `node --check exit ${r.status}`})`);
      }
      if (/\.json$/.test(f.name)) {
        try { JSON.parse(text); } catch (e) { findings.push(`${f.name}: invalid JSON (${e.message})`); }
      }
    }
    inputs.push({ path: input, sha256: hash.digest("hex"), files: files.length });
  }
  return { inputs, findings, missing, malformed: null };
}
