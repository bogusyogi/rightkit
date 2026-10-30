// The declared compile inputs a RustRight broker captured for a compile (RIGHTKIT_COMPILE_INPUTS): the gate judges exactly
// those bytes, where the broker copied them, so what is checked is what the compile reads. The tree's tracked files are
// judged from the commit as always; the declared inputs are the ignored paths (a built `dist`, generated assets) the
// commit does not hold, listed in the captured tree's `.rustright/compile-inputs.json` (the legacy `.rightkit/compile-inputs.json` is read only when
// that is absent, as the broker's declared_inputs does).
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { spawnSync } from "./background-process.mjs";
import { SECRET_PATTERNS } from "./publish-cargo.mjs";
import { COMPILE_INPUTS, LEGACY_COMPILE_INPUTS, overlapsDeclaration } from "./snapshot-request.mjs";

/** The current declaration file. Every finding about a malformed declaration starts with `${COMPILE_INPUTS_FILE}:` (callers filter on it), the legacy file's too. */
export const COMPILE_INPUTS_FILE = COMPILE_INPUTS;
export const LEGACY_COMPILE_INPUTS_FILE = LEGACY_COMPILE_INPUTS;

/** The declaration file under `root` the broker would read (the current one, else the legacy one), or null; a link or an unreadable path is an Error. */
function declarationOf(root) {
  for (const name of [COMPILE_INPUTS_FILE, LEGACY_COMPILE_INPUTS_FILE]) {
    let at = root;
    let missing = false;
    for (const part of name.split("/")) {
      at = path.join(at, part);
      let st;
      try { st = fs.lstatSync(at); } catch (e) { if (e.code === "ENOENT") { missing = true; break; } throw e; }
      if (st.isSymbolicLink()) throw new Error(`${name} runs through a link at ${at}`);
    }
    if (!missing) return { name, at };
  }
  return null;
}

/**
 * Files of one declared input in the broker's order (children by name), refusing links; [{ name, abs }]. A child's hashed name is its parent's name
 * joined with the HOST's separator, as the broker's `Path::join` does (`\` on Windows), so `platform` names the host whose digest this is.
 */
function inputFiles(root, input, platform = process.platform) {
  const sep = platform === "win32" ? "\\" : "/";
  const out = [];
  const walk = (abs, name) => {
    const st = fs.lstatSync(abs);
    if (st.isSymbolicLink()) throw new Error(`declared compile input holds a symbolic link at ${name}`);
    if (st.isDirectory()) {
      for (const child of fs.readdirSync(abs).sort((a, b) => Buffer.compare(Buffer.from(a), Buffer.from(b)))) walk(path.join(abs, child), `${name}${sep}${child}`);
    } else if (st.isFile()) out.push({ name, abs });
  };
  walk(path.join(root, input), input);
  return out;
}

/**
 * Judges the captured declared inputs under `root`: each declared path must exist without links; every `.js/.mjs/.cjs`
 * file parses (`node --check`), every `.json` file parses, and no file carries a secret. Returns { inputs: [{ path, sha256,
 * files }], findings: [string] }; `sha256` is computed as the broker records it (name, NUL, bytes, NUL per file, in order),
 * so the receipt and the gate name the same bytes.
 */
export function judgeCompileInputs(root, { spawn = spawnSync, execPath = process.execPath, platform = process.platform } = {}) {
  const findings = [];
  let declared = [];
  let found;
  try { found = declarationOf(root); } catch (e) { return { inputs: [], findings: [`${COMPILE_INPUTS_FILE}: ${e.message}`] }; }
  if (found === null) return { inputs: [], findings };
  const file = found.at;
  const label = found.name === COMPILE_INPUTS_FILE ? COMPILE_INPUTS_FILE : `${COMPILE_INPUTS_FILE}: ${found.name}`;
  try {
    declared = JSON.parse(fs.readFileSync(file, "utf8")).inputs;
    if (!Array.isArray(declared) || declared.some((p) => typeof p !== "string")) throw new Error("`inputs` must be a list of paths");
  } catch (e) {
    return { inputs: [], findings: [`${label}: ${e.message}`] };
  }
  const inputs = [];
  for (const input of declared) {
    if (path.isAbsolute(input) || input.split(/[\\/]/).some((part) => part === ".." || part === "." || part === "")) { findings.push(`${input}: not a plain relative path`); continue; }
    if (overlapsDeclaration(input.split(/[\\/]/))) { findings.push(`${input}: overlaps the declaration file itself`); continue; }
    let files;
    try { files = inputFiles(root, input, platform); } catch (e) { findings.push(`${input}: ${e.code === "ENOENT" ? "missing from the captured tree" : e.message}`); continue; }
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
  return { inputs, findings };
}
